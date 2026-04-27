use iced::widget::operation::scroll_to;
use iced::widget::scrollable::{AbsoluteOffset, Viewport};
use iced::widget::Id as WidgetId;
use iced::{
    widget::{
        button, checkbox, column, container, pick_list, row, rule, scrollable, text, tooltip,
        Column, Space,
    },
    Element, Length, Task,
};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::{drives::MountMode, Rest};

/// Stable ID for the main file-list scrollable widget
const FILE_LIST_SCROLLABLE_ID: &str = "file_browser_list";

use crate::archive::{extract_zip_to_dir, MAX_ZIP_EXTRACT_BYTES};
use crate::dir_preview::{self, ContentPreview};
use crate::disk_image::{self, DiskInfo, FileType};
use crate::net_utils::REST_TIMEOUT_SECS;
use crate::pdf_preview;
/// Longer timeout for run_disk which includes boot delays
const RUN_DISK_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone)]
pub enum FileBrowserMessage {
    SelectDirectory,
    DirectorySelected(PathBuf),
    FileSelected(PathBuf),
    ToggleFileCheck(PathBuf, bool),
    SelectAll,
    SelectNone,
    MountDisk(PathBuf, String, MountMode),
    MountCompleted(Result<(), String>),
    RunDisk(PathBuf, String), // Mount, reset, load and run
    RunDiskCompleted(Result<(), String>),
    LoadAndRun(PathBuf),
    LoadCompleted(Result<(), String>),
    RefreshFiles,
    NavigateUp,
    DriveSelected(DriveOption),
    NavigateToPath(PathBuf),
    /// Internal: sent by the framework after a directory change to restore scroll position
    RestoreScrollOffset(f32),
    /// Internal: tracks current scroll offset so NavigateUp can restore it
    Scrolled(Viewport),
    FilterChanged(String),
    NavigateToAssemblyDir,
    // ZIP extraction: extract archive to a sibling subfolder then navigate there
    ExtractZip(PathBuf),
    ZipExtracted(Result<PathBuf, String>),
    // Disk info popup
    ShowDiskInfo(PathBuf),
    DiskInfoLoaded(Result<DiskInfo, String>),
    CloseDiskInfo,
    // Content preview popup (text/image files)
    ShowContentPreview(PathBuf),
    ContentPreviewLoaded(Result<ContentPreview, String>),
    CloseContentPreview,
    // Drive enable dialog
    /// Check if target drive is enabled before mounting/running; carries the pending action
    CheckDriveBeforeAction(PendingDriveAction),
    DriveCheckComplete(Result<bool, String>, PendingDriveAction),
    ConfirmEnableDrive,
    CancelEnableDrive,
    EnableDriveComplete(Result<(), String>),
    SortBy(crate::file_types::SortColumn),
    PlaySid(PathBuf),
    PlayMod(PathBuf),
    // Local delete
    DeleteChecked,
    DeleteConfirmed,
    DeleteCancelled,
    DeleteComplete(Result<String, String>),
    // Local mkdir
    ShowCreateDir,
    CreateDirNameChanged(String),
    CreateDirConfirm,
    CreateDirCancel,
    // Local disk image creation
    ShowCreateDisk,
    CloseCreateDisk,
    CreateDiskNameChanged(String),
    CreateDiskIdChanged(String),
    CreateDiskTypeChanged(crate::ftp_ops::DiskCreateType),
    CreateDiskConfirm,
}

/// The action to execute once we know the drive is enabled
#[derive(Debug, Clone)]
pub enum PendingDriveAction {
    Mount(PathBuf, String, MountMode),
    Run(PathBuf, String),
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriveOption {
    A,
    B,
}

impl std::fmt::Display for DriveOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriveOption::A => write!(f, "Drive A (8)"),
            DriveOption::B => write!(f, "Drive B (9)"),
        }
    }
}

impl DriveOption {
    pub fn to_drive_string(&self) -> String {
        match self {
            DriveOption::A => "a".to_string(),
            DriveOption::B => "b".to_string(),
        }
    }

    pub fn get_all() -> Vec<DriveOption> {
        vec![DriveOption::A, DriveOption::B]
    }
}

pub struct FileBrowser {
    current_directory: PathBuf,
    files: Vec<FileEntry>,
    selected_file: Option<PathBuf>,
    checked_files: HashSet<PathBuf>,
    selected_drive: DriveOption,
    status_message: Option<String>,
    filter: String,
    // True while an async operation (e.g. ZIP extraction) is in progress
    is_loading: bool,
    // Disk info popup state
    disk_info_popup: Option<DiskInfo>,
    disk_info_path: Option<PathBuf>,
    disk_info_loading: bool,
    // Rendered C64-style PETSCII listing image (PNG bytes)
    disk_listing_image: Option<Vec<u8>>,
    // Content preview popup state (text/image files)
    content_preview: Option<ContentPreview>,
    content_preview_path: Option<PathBuf>,
    content_preview_loading: bool,
    // Scroll position memory: directory → (y_offset, last selected child path)
    scroll_memory: HashMap<PathBuf, (f32, Option<PathBuf>)>,
    // The child directory we most recently entered (used to highlight it on NavigateUp)
    last_entered_child: Option<PathBuf>,
    // Drive enable dialog: Some(...) when we're asking the user to enable a disabled drive
    drive_enable_dialog: Option<(DriveOption, PendingDriveAction)>,
    // Sort state for column headers
    sort_column: crate::file_types::SortColumn,
    sort_order: crate::file_types::SortOrder,
    // Delete confirmation: paths pending deletion
    delete_pending: Option<Vec<PathBuf>>,
    // Create directory dialog
    show_create_dir: bool,
    create_dir_name: String,
    // Create disk image dialog
    show_create_disk: bool,
    create_disk_name: String,
    create_disk_id: String,
    create_disk_type: crate::ftp_ops::DiskCreateType,
}

impl FileBrowser {
    /// Create a new FileBrowser with an optional starting directory.
    /// If start_dir is None or invalid, defaults to the user's home directory.
    pub fn new(start_dir: Option<PathBuf>) -> Self {
        // Use provided path if it exists and is a directory, otherwise fall back to home
        let initial_dir = start_dir
            .filter(|p| p.exists() && p.is_dir())
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));

        let mut browser = Self {
            current_directory: initial_dir.clone(),
            files: Vec::new(),
            selected_file: None,
            checked_files: HashSet::new(),
            selected_drive: DriveOption::A,
            status_message: None,
            filter: String::new(),
            is_loading: false,
            disk_info_popup: None,
            disk_info_path: None,
            disk_info_loading: false,
            disk_listing_image: None,
            content_preview: None,
            content_preview_path: None,
            content_preview_loading: false,
            scroll_memory: HashMap::new(),
            last_entered_child: None,
            drive_enable_dialog: None,
            sort_column: crate::file_types::SortColumn::Name,
            sort_order: crate::file_types::SortOrder::Ascending,
            delete_pending: None,
            show_create_dir: false,
            create_dir_name: String::new(),
            show_create_disk: false,
            create_disk_name: "NEWDISK".to_string(),
            create_disk_id: "01 2A".to_string(),
            create_disk_type: crate::ftp_ops::DiskCreateType::D64,
        };
        browser.load_directory(&initial_dir);
        browser
    }

    pub fn update(
        &mut self,
        message: FileBrowserMessage,
        connection: Option<Arc<Mutex<Rest>>>,
        host: Option<String>,
        password: Option<String>,
    ) -> Task<FileBrowserMessage> {
        match message {
            FileBrowserMessage::SelectDirectory => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                |result| {
                    if let Some(path) = result {
                        FileBrowserMessage::DirectorySelected(path)
                    } else {
                        FileBrowserMessage::RefreshFiles
                    }
                },
            ),
            FileBrowserMessage::DirectorySelected(path) => {
                self.load_directory(&path);
                self.current_directory = path;
                self.checked_files.clear();
                self.status_message = None;
                self.last_entered_child = None;
                Task::none()
            }
            FileBrowserMessage::FileSelected(path) => {
                if path.is_dir() {
                    // Remember which child we're entering, so NavigateUp can highlight it
                    self.last_entered_child = Some(path.clone());
                    // Save current scroll position under the current directory
                    // (actual y-offset is stored when RestoreScrollOffset arrives;
                    //  here we just make sure an entry exists so NavigateUp can read it)
                    self.scroll_memory
                        .entry(self.current_directory.clone())
                        .or_insert((0.0, None))
                        .1 = Some(path.clone());
                    self.load_directory(&path);
                    self.current_directory = path;
                    self.checked_files.clear();
                } else {
                    self.selected_file = Some(path);
                }
                Task::none()
            }
            FileBrowserMessage::NavigateToPath(path) => {
                if path.is_dir() {
                    self.last_entered_child = Some(path.clone());
                    self.scroll_memory
                        .entry(self.current_directory.clone())
                        .or_insert((0.0, None))
                        .1 = Some(path.clone());
                    self.load_directory(&path);
                    self.current_directory = path;
                    self.checked_files.clear();
                }
                Task::none()
            }
            FileBrowserMessage::MountDisk(path, drive, mode) => {
                if connection.is_some() {
                    let action = PendingDriveAction::Mount(path, drive, mode);
                    return Task::done(FileBrowserMessage::CheckDriveBeforeAction(action));
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }
            FileBrowserMessage::MountCompleted(result) => {
                match result {
                    Ok(_) => {
                        self.status_message = Some("Disk mounted successfully!".to_string());
                        log::info!("Disk mounted successfully");
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Mount failed: {}", e));
                        log::error!("Mount failed: {}", e);
                    }
                }
                Task::none()
            }
            FileBrowserMessage::RunDisk(path, drive) => {
                if connection.is_some() {
                    let action = PendingDriveAction::Run(path, drive);
                    return Task::done(FileBrowserMessage::CheckDriveBeforeAction(action));
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }
            FileBrowserMessage::RunDiskCompleted(result) => {
                match result {
                    Ok(_) => {
                        self.status_message = Some("Disk loaded and running!".to_string());
                        log::info!("Disk run successful");
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Run failed: {}", e));
                        log::error!("Run failed: {}", e);
                    }
                }
                Task::none()
            }
            FileBrowserMessage::LoadAndRun(path) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Loading {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Task::perform(
                        load_and_run_async(conn, path),
                        FileBrowserMessage::LoadCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }
            FileBrowserMessage::LoadCompleted(result) => {
                match result {
                    Ok(_) => {
                        self.status_message = Some("Loaded successfully!".to_string());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Load failed: {}", e));
                        log::error!("Load failed: {}", e);
                    }
                }
                Task::none()
            }
            FileBrowserMessage::RefreshFiles => {
                self.load_directory(&self.current_directory.clone());
                self.status_message = None;
                // Restore scroll to where user was before refresh
                let saved_y = self
                    .scroll_memory
                    .get(&self.current_directory)
                    .map(|(y, _)| *y)
                    .unwrap_or(0.0);
                if saved_y > 0.0 {
                    Task::done(FileBrowserMessage::RestoreScrollOffset(saved_y))
                } else {
                    Task::none()
                }
            }
            FileBrowserMessage::NavigateUp => {
                if let Some(parent) = self.current_directory.parent() {
                    let parent_path = parent.to_path_buf();
                    // Remember which child we're coming back from so it can be highlighted
                    self.last_entered_child = Some(self.current_directory.clone());
                    self.load_directory(&parent_path);
                    self.current_directory = parent_path.clone();
                    // Also restore selected_file to the child we came from so the
                    // highlight logic in view_file_entry picks it up
                    self.selected_file = self.last_entered_child.clone();

                    // Restore saved scroll offset if we have one
                    let saved_y = self
                        .scroll_memory
                        .get(&parent_path)
                        .map(|(y, _)| *y)
                        .unwrap_or(0.0);

                    if saved_y > 0.0 {
                        return Task::done(FileBrowserMessage::RestoreScrollOffset(saved_y));
                    }
                }
                Task::none()
            }
            FileBrowserMessage::RestoreScrollOffset(y) => scroll_to(
                WidgetId::new(FILE_LIST_SCROLLABLE_ID),
                AbsoluteOffset { x: 0.0, y },
            ),
            FileBrowserMessage::Scrolled(viewport) => {
                let y = viewport.absolute_offset().y;
                self.scroll_memory
                    .entry(self.current_directory.clone())
                    .or_insert((0.0, None))
                    .0 = y;
                Task::none()
            }
            FileBrowserMessage::DriveSelected(drive) => {
                self.selected_drive = drive;
                Task::none()
            }
            FileBrowserMessage::ToggleFileCheck(path, checked) => {
                if checked {
                    self.checked_files.insert(path);
                } else {
                    self.checked_files.remove(&path);
                }
                Task::none()
            }
            FileBrowserMessage::SelectAll => {
                for file in &self.files {
                    self.checked_files.insert(file.path.clone());
                }
                Task::none()
            }
            FileBrowserMessage::SelectNone => {
                self.checked_files.clear();
                Task::none()
            }
            FileBrowserMessage::FilterChanged(value) => {
                self.filter = value;
                Task::none()
            }
            FileBrowserMessage::NavigateToAssemblyDir => {
                if let Some(config_dir) = dirs::config_dir() {
                    let base = config_dir.join("ultimate64-manager");
                    let outcome = migrate_csdb_to_assembly(&base);
                    let asm_dir = base.join("Assembly64");

                    // Create the folder if neither it nor a CSDB legacy
                    // exists — clicking the button should always navigate
                    // somewhere reasonable, even on a clean install.
                    if !asm_dir.exists() {
                        if let Err(e) = std::fs::create_dir_all(&asm_dir) {
                            self.status_message =
                                Some(format!("Could not create Assembly64 folder: {}", e));
                            return Task::none();
                        }
                    }

                    self.load_directory(&asm_dir);
                    self.current_directory = asm_dir;
                    self.checked_files.clear();
                    self.last_entered_child = None;
                    self.status_message = Some(match outcome {
                        MigrationOutcome::Renamed => {
                            "Migrated CSDB downloads to Assembly64".to_string()
                        }
                        MigrationOutcome::BothExisted => {
                            "Both CSDB and Assembly64 folders exist — merge manually if needed"
                                .to_string()
                        }
                        MigrationOutcome::Failed(kind) => {
                            format!("Could not migrate CSDB folder ({:?})", kind)
                        }
                        MigrationOutcome::Nothing => String::new(),
                    });
                } else {
                    self.status_message = Some("Could not determine config directory".to_string());
                }
                Task::none()
            }
            // Extract a ZIP archive into a sibling subfolder (named after the ZIP stem),
            // then navigate into that folder so the contents are immediately visible.
            FileBrowserMessage::ExtractZip(zip_path) => {
                // Reject files above the size limit to avoid accidentally unpacking
                // massive TOSEC collections or similar large ZIPs
                match std::fs::metadata(&zip_path) {
                    Ok(meta) if meta.len() > MAX_ZIP_EXTRACT_BYTES => {
                        self.status_message = Some(format!(
                            "ZIP too large ({:.1} MB). Maximum allowed is {} MB.",
                            meta.len() as f64 / (1024.0 * 1024.0),
                            MAX_ZIP_EXTRACT_BYTES / (1024 * 1024)
                        ));
                        return Task::none();
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Cannot read ZIP metadata: {}", e));
                        return Task::none();
                    }
                    _ => {}
                }

                // Target dir = same parent as the ZIP, subfolder named after the ZIP stem
                let zip_stem = zip_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("extracted")
                    .to_string();
                let target_dir = match zip_path.parent() {
                    Some(parent) => parent.join(&zip_stem),
                    None => {
                        self.status_message = Some("Cannot determine parent directory".to_string());
                        return Task::none();
                    }
                };

                let zip_name = zip_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("archive.zip")
                    .to_string();

                self.is_loading = true;
                self.status_message = Some(format!("Extracting {}...", zip_name));

                Task::perform(
                    extract_zip_file_async(zip_path, target_dir),
                    FileBrowserMessage::ZipExtracted,
                )
            }
            FileBrowserMessage::ZipExtracted(result) => {
                self.is_loading = false;
                match result {
                    Ok(dir) => {
                        self.status_message = Some(format!("Extracted to: {}", dir.display()));
                        // Navigate into the freshly extracted folder
                        self.load_directory(&dir);
                        self.current_directory = dir;
                        self.checked_files.clear();
                        self.last_entered_child = None;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Extraction failed: {}", e));
                    }
                }
                Task::none()
            }
            // Disk info popup messages
            FileBrowserMessage::ShowDiskInfo(path) => {
                self.disk_info_loading = true;
                self.disk_info_path = Some(path.clone());
                Task::perform(
                    async move { load_disk_info_async(path).await },
                    FileBrowserMessage::DiskInfoLoaded,
                )
            }
            FileBrowserMessage::DiskInfoLoaded(result) => {
                self.disk_info_loading = false;
                match result {
                    Ok(info) => {
                        // Render a C64-style PETSCII listing image
                        self.disk_listing_image =
                            Some(dir_preview::render_disk_listing_image(&info));
                        self.disk_info_popup = Some(info);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to read disk: {}", e));
                        self.disk_info_path = None;
                    }
                }
                Task::none()
            }
            FileBrowserMessage::CloseDiskInfo => {
                self.disk_info_popup = None;
                self.disk_info_path = None;
                self.disk_listing_image = None;
                Task::none()
            }
            // Content preview popup messages (text/image files)
            FileBrowserMessage::ShowContentPreview(path) => {
                self.content_preview_loading = true;
                self.content_preview_path = Some(path.clone());
                Task::perform(
                    async move { load_content_preview_async(path).await },
                    FileBrowserMessage::ContentPreviewLoaded,
                )
            }
            FileBrowserMessage::ContentPreviewLoaded(result) => {
                self.content_preview_loading = false;
                match result {
                    Ok(preview) => {
                        self.content_preview = Some(preview);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to load content: {}", e));
                        self.content_preview_path = None;
                    }
                }
                Task::none()
            }
            FileBrowserMessage::CloseContentPreview => {
                self.content_preview = None;
                self.content_preview_path = None;
                Task::none()
            }

            // ── Drive enable dialog ──────────────────────────────
            FileBrowserMessage::CheckDriveBeforeAction(action) => {
                // Determine which drive letter is targeted
                let drive_letter = match &action {
                    PendingDriveAction::Mount(_, d, _) => d.clone(),
                    PendingDriveAction::Run(_, d) => d.clone(),
                };

                let effective_host = host.filter(|h| !h.is_empty());
                log::info!(
                    "CheckDriveBeforeAction: drive={} host={:?}",
                    drive_letter,
                    effective_host
                );

                if let Some(h) = effective_host {
                    self.is_loading = true;
                    self.status_message = Some("Checking drive status…".to_string());
                    Task::perform(
                        check_drive_enabled_async(h, drive_letter, password),
                        move |result| {
                            FileBrowserMessage::DriveCheckComplete(result, action.clone())
                        },
                    )
                } else {
                    log::warn!("CheckDriveBeforeAction: no host, skipping drive check");
                    self.dispatch_action(action, connection)
                }
            }

            FileBrowserMessage::DriveCheckComplete(result, action) => {
                self.is_loading = false;
                log::info!("DriveCheckComplete: {:?}", result);
                match result {
                    Ok(true) => {
                        // Drive already enabled — proceed immediately
                        self.status_message = None;
                        self.dispatch_action(action, connection)
                    }
                    Ok(false) => {
                        // Drive is disabled — show confirmation dialog
                        let drive_letter = match &action {
                            PendingDriveAction::Mount(_, d, _) => d.clone(),
                            PendingDriveAction::Run(_, d) => d.clone(),
                        };
                        let drive_opt = if drive_letter == "a" {
                            DriveOption::A
                        } else {
                            DriveOption::B
                        };
                        self.drive_enable_dialog = Some((drive_opt, action));
                        self.status_message = None;
                        Task::none()
                    }
                    Err(e) => {
                        // Can't check — proceed anyway and let mount fail naturally
                        log::warn!("Could not check drive status: {}", e);
                        self.status_message = None;
                        self.dispatch_action(action, connection)
                    }
                }
            }

            FileBrowserMessage::ConfirmEnableDrive => {
                if let Some((drive_opt, action)) = self.drive_enable_dialog.take() {
                    let drive_letter = drive_opt.to_drive_string();
                    if let Some(h) = host {
                        self.is_loading = true;
                        self.status_message = Some(format!(
                            "Enabling Drive {} temporarily…",
                            drive_letter.to_uppercase()
                        ));
                        // Store the action back so EnableDriveComplete can dispatch it
                        self.drive_enable_dialog = Some((drive_opt, action));
                        Task::perform(
                            enable_drive_async(h, drive_letter, password),
                            FileBrowserMessage::EnableDriveComplete,
                        )
                    } else {
                        Task::none()
                    }
                } else {
                    Task::none()
                }
            }

            FileBrowserMessage::CancelEnableDrive => {
                self.drive_enable_dialog = None;
                self.status_message = Some("Cancelled".to_string());
                Task::none()
            }

            FileBrowserMessage::EnableDriveComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(()) => {
                        self.status_message =
                            Some("Drive enabled temporarily (reboot to restore)".to_string());
                        // Now dispatch the original action that was waiting
                        if let Some((_, action)) = self.drive_enable_dialog.take() {
                            return self.dispatch_action(action, connection);
                        }
                    }
                    Err(e) => {
                        self.drive_enable_dialog = None;
                        self.status_message = Some(format!("Enable drive failed: {}", e));
                    }
                }
                Task::none()
            }

            FileBrowserMessage::PlaySid(path) => {
                if let (Some(_host), Some(conn)) = (host.clone(), connection.clone()) {
                    Task::perform(
                        async move {
                            let data = tokio::fs::read(&path)
                                .await
                                .map_err(|e| format!("Failed to read file: {}", e))?;
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let c = conn.blocking_lock();
                                    c.sid_play(&data, None).map_err(|e| e.to_string())
                                }),
                            )
                            .await;
                            match result {
                                Ok(Ok(inner)) => inner,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => Err("Play timed out".to_string()),
                            }
                        },
                        |result| match result {
                            Ok(_) => FileBrowserMessage::LoadCompleted(Ok(())),
                            Err(e) => FileBrowserMessage::LoadCompleted(Err(e)),
                        },
                    )
                } else {
                    self.status_message = Some("Not connected to device".to_string());
                    Task::none()
                }
            }
            FileBrowserMessage::PlayMod(path) => {
                if let (Some(_host), Some(conn)) = (host.clone(), connection.clone()) {
                    Task::perform(
                        async move {
                            let data = tokio::fs::read(&path)
                                .await
                                .map_err(|e| format!("Failed to read file: {}", e))?;
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let c = conn.blocking_lock();
                                    c.mod_play(&data).map_err(|e| e.to_string())
                                }),
                            )
                            .await;
                            match result {
                                Ok(Ok(inner)) => inner,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => Err("Play timed out".to_string()),
                            }
                        },
                        |result| match result {
                            Ok(_) => FileBrowserMessage::LoadCompleted(Ok(())),
                            Err(e) => FileBrowserMessage::LoadCompleted(Err(e)),
                        },
                    )
                } else {
                    self.status_message = Some("Not connected to device".to_string());
                    Task::none()
                }
            }
            FileBrowserMessage::SortBy(col) => {
                if self.sort_column == col {
                    self.sort_order = self.sort_order.toggle();
                } else {
                    self.sort_column = col;
                    self.sort_order = crate::file_types::SortOrder::Ascending;
                }
                self.sort_files();
                Task::none()
            }

            // ── Local delete ─────────────────────────────────────────────────
            FileBrowserMessage::DeleteChecked => {
                let checked: Vec<PathBuf> = self.checked_files.iter().cloned().collect();
                if checked.is_empty() {
                    self.status_message = Some("No files selected".to_string());
                    return Task::none();
                }
                self.delete_pending = Some(checked);
                Task::none()
            }
            FileBrowserMessage::DeleteConfirmed => {
                if let Some(paths) = self.delete_pending.take() {
                    return Task::perform(
                        async move {
                            let mut deleted = 0;
                            let mut errors = Vec::new();
                            for path in &paths {
                                let result = if path.is_dir() {
                                    tokio::fs::remove_dir_all(path).await
                                } else {
                                    tokio::fs::remove_file(path).await
                                };
                                match result {
                                    Ok(_) => deleted += 1,
                                    Err(e) => errors.push(format!(
                                        "{}: {}",
                                        path.file_name().unwrap_or_default().to_string_lossy(),
                                        e
                                    )),
                                }
                            }
                            let mut msg = format!("Deleted {} item(s)", deleted);
                            if !errors.is_empty() {
                                msg.push_str(&format!(" ({} errors)", errors.len()));
                            }
                            Ok(msg)
                        },
                        FileBrowserMessage::DeleteComplete,
                    );
                }
                Task::none()
            }
            FileBrowserMessage::DeleteCancelled => {
                self.delete_pending = None;
                Task::none()
            }
            FileBrowserMessage::DeleteComplete(result) => {
                match &result {
                    Ok(msg) => self.status_message = Some(msg.clone()),
                    Err(e) => self.status_message = Some(format!("Error: {}", e)),
                }
                self.checked_files.clear();
                self.load_directory(&self.current_directory.clone());
                Task::none()
            }

            // ── Local mkdir ──────────────────────────────────────────────────
            FileBrowserMessage::ShowCreateDir => {
                self.show_create_dir = true;
                self.create_dir_name = String::new();
                Task::none()
            }
            FileBrowserMessage::CreateDirNameChanged(name) => {
                self.create_dir_name = name;
                Task::none()
            }
            FileBrowserMessage::CreateDirConfirm => {
                if self.create_dir_name.trim().is_empty() {
                    return Task::none();
                }
                let dir_path = self.current_directory.join(self.create_dir_name.trim());
                self.show_create_dir = false;
                match std::fs::create_dir(&dir_path) {
                    Ok(_) => {
                        self.status_message =
                            Some(format!("Created: {}", self.create_dir_name.trim()));
                        self.load_directory(&self.current_directory.clone());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error: {}", e));
                    }
                }
                Task::none()
            }
            FileBrowserMessage::CreateDirCancel => {
                self.show_create_dir = false;
                Task::none()
            }

            // ── Local create disk image ──────────────────────────────────
            FileBrowserMessage::ShowCreateDisk => {
                self.show_create_disk = true;
                self.create_disk_name = "NEWDISK".to_string();
                self.create_disk_id = "01 2A".to_string();
                Task::none()
            }
            FileBrowserMessage::CloseCreateDisk => {
                self.show_create_disk = false;
                Task::none()
            }
            FileBrowserMessage::CreateDiskNameChanged(name) => {
                self.create_disk_name = name.to_uppercase();
                Task::none()
            }
            FileBrowserMessage::CreateDiskIdChanged(id) => {
                self.create_disk_id = id;
                Task::none()
            }
            FileBrowserMessage::CreateDiskTypeChanged(dt) => {
                self.create_disk_type = dt;
                Task::none()
            }
            FileBrowserMessage::CreateDiskConfirm => {
                use crate::disk_image;
                let name = self.create_disk_name.trim().to_string();
                if name.is_empty() {
                    return Task::none();
                }
                let id = self.create_disk_id.trim().to_string();
                let safe_name = name.replace(' ', "_");
                let (ext, data) = match self.create_disk_type {
                    crate::ftp_ops::DiskCreateType::D64 => {
                        ("d64", disk_image::build_blank_d64(&name, &id))
                    }
                    crate::ftp_ops::DiskCreateType::D71 => {
                        ("d71", disk_image::build_blank_d71(&name, &id))
                    }
                    crate::ftp_ops::DiskCreateType::D81 => {
                        ("d81", disk_image::build_blank_d81(&name, &id))
                    }
                };
                let filename = format!("{}.{}", safe_name, ext);
                let file_path = self.current_directory.join(&filename);
                match std::fs::write(&file_path, &data) {
                    Ok(_) => {
                        self.status_message = Some(format!("Created: {}", filename));
                        self.show_create_disk = false;
                        self.load_directory(&self.current_directory.clone());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error: {}", e));
                    }
                }
                Task::none()
            }
        }
    }
    #[allow(dead_code)]
    pub fn get_selected_file(&self) -> Option<&PathBuf> {
        self.selected_file.as_ref()
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn get_checked_files(&self) -> Vec<PathBuf> {
        self.checked_files.iter().cloned().collect()
    }

    pub fn clear_checked(&mut self) {
        self.checked_files.clear();
    }

    pub fn get_current_directory(&self) -> &PathBuf {
        &self.current_directory
    }

    /// Build the compact navigation row: [Up] path/to/current/dir...
    fn build_nav_row(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let path_str = self.current_directory.to_string_lossy();
        let display_path = if path_str.len() > 45 {
            format!("...{}", &path_str[path_str.len() - 43..])
        } else {
            path_str.to_string()
        };

        row![
            tooltip(
                button(text("⬆").size(fs.normal))
                    .on_press(FileBrowserMessage::NavigateUp)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                "Go to parent folder",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
            text(display_path).size(fs.small).width(Length::Fill),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// Build the quick navigation row: [Browse] [CSDb] [Home]
    fn build_quick_nav_row(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        row![
            tooltip(
                button(text("🏠 Home").size(fs.small))
                    .on_press(FileBrowserMessage::NavigateToPath(
                        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
                    ))
                    .padding([2, 6])
                    .style(crate::styles::nav_button),
                "Go to home directory",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
            tooltip(
                button(text("Assembly64").size(fs.small))
                    .on_press(FileBrowserMessage::NavigateToAssemblyDir)
                    .padding([2, 6])
                    .style(crate::styles::nav_button),
                "Go to Assembly64 downloads folder",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
            tooltip(
                button(text("🔍 Browse").size(fs.small))
                    .on_press(FileBrowserMessage::SelectDirectory)
                    .padding([2, 6])
                    .style(crate::styles::nav_button),
                "Choose a different folder",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
        ]
        .spacing(3)
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// Build the status bar: file count | selection | Drive picker
    fn build_status_bar(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let checked_count = self.checked_files.len();
        let file_count = self.files.len();

        let mut items = row![].spacing(8).align_y(iced::Alignment::Center);

        // Loading indicator or status message
        if self.disk_info_loading || self.content_preview_loading || self.is_loading {
            items = items.push(text("Loading...").size(fs.tiny));
        } else if let Some(msg) = &self.status_message {
            items = items.push(text(msg).size(fs.tiny));
        }

        items = items.push(text(format!("{} files", file_count)).size(fs.tiny));

        if checked_count > 0 {
            items = items.push(text("|").size(fs.tiny));
            items = items.push(text(format!("{} sel", checked_count)).size(fs.tiny));
        }

        items = items.push(Space::new().width(Length::Fill));
        items = items.push(text("Drive:").size(fs.tiny));
        items = items.push(
            pick_list(
                DriveOption::get_all(),
                Some(self.selected_drive.clone()),
                FileBrowserMessage::DriveSelected,
            )
            .placeholder("Drive")
            .text_size(fs.tiny)
            .width(Length::Fixed(95.0)),
        );

        items.into()
    }

    /// Build column headers for the file list (Name, Size, Type)
    fn build_column_headers(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        use crate::file_types::SortColumn;
        let fs = crate::styles::FontSizes::from_base(font_size);

        row![
            Space::new().width(24), // checkbox space
            button(
                text(format!(
                    "Name{}",
                    if self.sort_column == SortColumn::Name {
                        self.sort_order.indicator()
                    } else {
                        ""
                    }
                ))
                .size(fs.small),
            )
            .on_press(FileBrowserMessage::SortBy(SortColumn::Name))
            .padding([2, 4])
            .style(button::text),
            Space::new().width(Length::Fill),
            button(
                text(format!(
                    "Size{}",
                    if self.sort_column == SortColumn::Size {
                        self.sort_order.indicator()
                    } else {
                        ""
                    }
                ))
                .size(fs.small),
            )
            .on_press(FileBrowserMessage::SortBy(SortColumn::Size))
            .padding([2, 4])
            .style(button::text)
            .width(Length::Fixed(65.0)),
            button(
                text(format!(
                    "Type{}",
                    if self.sort_column == SortColumn::Type {
                        self.sort_order.indicator()
                    } else {
                        ""
                    }
                ))
                .size(fs.small),
            )
            .on_press(FileBrowserMessage::SortBy(SortColumn::Type))
            .padding([2, 4])
            .width(Length::Fixed(35.0))
            .style(button::text),
            Space::new().width(Length::Shrink), // action buttons space
        ]
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub fn view(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        // If drive enable dialog is open, show it instead of the file list
        if let Some((drive_opt, _)) = &self.drive_enable_dialog {
            let drive_num = match drive_opt {
                DriveOption::A => "8",
                DriveOption::B => "9",
            };
            let drive_letter = match drive_opt {
                DriveOption::A => "A",
                DriveOption::B => "B",
            };
            let dialog = container(
                column![
                    text(format!(
                        "Drive {} (device {}) is currently disabled.",
                        drive_letter, drive_num
                    ))
                    .size(fs.normal),
                    text("Enable it temporarily? (reboot restores your original settings)")
                        .size(fs.small),
                    row![
                        button(text(format!("Enable Drive {}", drive_letter)).size(fs.small))
                            .on_press(FileBrowserMessage::ConfirmEnableDrive)
                            .padding([5, 15])
                            .style(button::secondary),
                        button(text("Cancel").size(fs.small))
                            .on_press(FileBrowserMessage::CancelEnableDrive)
                            .padding([5, 15])
                            .style(button::secondary),
                    ]
                    .spacing(10),
                ]
                .spacing(12)
                .padding(20),
            )
            .style(crate::styles::subtle_tooltip)
            .width(Length::Fill);

            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into();
        }

        // If disk info popup is open, show it instead of the file list
        if let Some(disk_info) = &self.disk_info_popup {
            let popup = self.view_disk_info_popup(disk_info, font_size);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                popup,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        } else if let Some(content_preview) = &self.content_preview {
            // If content preview popup is open, show it instead of the file list
            let popup = self.view_content_preview_popup(content_preview, font_size);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                popup,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        } else if let Some(ref paths) = self.delete_pending {
            // Delete confirmation dialog
            let summary = paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            let dialog = container(
                column![
                    text(format!("Delete {} item(s)?", paths.len())).size(fs.normal),
                    text(if summary.len() > 80 {
                        format!("{}...", &summary[..77])
                    } else {
                        summary
                    })
                    .size(fs.small),
                    row![
                        button(text("Delete").size(fs.small))
                            .on_press(FileBrowserMessage::DeleteConfirmed)
                            .padding([5, 15])
                            .style(button::secondary),
                        button(text("Cancel").size(fs.small))
                            .on_press(FileBrowserMessage::DeleteCancelled)
                            .padding([5, 15])
                            .style(button::secondary),
                    ]
                    .spacing(10),
                ]
                .spacing(10)
                .padding(15),
            )
            .style(crate::styles::subtle_tooltip)
            .width(Length::Fill);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        } else if self.show_create_dir {
            // Create directory dialog
            let dialog = container(
                column![
                    text("Create Directory").size(fs.normal),
                    row![
                        text("Name:").size(fs.small),
                        iced::widget::text_input("directory name...", &self.create_dir_name)
                            .on_input(FileBrowserMessage::CreateDirNameChanged)
                            .on_submit(FileBrowserMessage::CreateDirConfirm)
                            .size(fs.small as f32)
                            .padding(4)
                            .width(Length::Fixed(200.0)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                    row![
                        button(text("Create").size(fs.small))
                            .on_press(FileBrowserMessage::CreateDirConfirm)
                            .padding([5, 15])
                            .style(button::secondary),
                        button(text("Cancel").size(fs.small))
                            .on_press(FileBrowserMessage::CreateDirCancel)
                            .padding([5, 15])
                            .style(button::secondary),
                    ]
                    .spacing(10),
                ]
                .spacing(10)
                .padding(15),
            )
            .style(crate::styles::subtle_tooltip)
            .width(Length::Fill);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        } else if self.show_create_disk {
            // Create disk image dialog
            let dim = iced::Color::from_rgb(0.55, 0.55, 0.6);
            let safe_name = self.create_disk_name.replace(' ', "_");
            let ext = match self.create_disk_type {
                crate::ftp_ops::DiskCreateType::D64 => "d64",
                crate::ftp_ops::DiskCreateType::D71 => "d71",
                crate::ftp_ops::DiskCreateType::D81 => "d81",
            };
            let preview_filename = format!("{}.{}", safe_name, ext);

            let dialog = container(
                column![
                    row![
                        text("Create New Disk Image").size(fs.normal),
                        Space::new().width(Length::Fill),
                        button(text("Cancel").size(fs.small))
                            .on_press(FileBrowserMessage::CloseCreateDisk)
                            .padding([4, 10])
                            .style(crate::styles::nav_button),
                    ]
                    .align_y(iced::Alignment::Center),
                    rule::horizontal(1),
                    row![
                        text("Format:")
                            .size(fs.normal)
                            .color(dim)
                            .width(Length::Fixed(80.0)),
                        button(
                            text(
                                if self.create_disk_type == crate::ftp_ops::DiskCreateType::D64 {
                                    "* D64"
                                } else {
                                    "  D64"
                                }
                            )
                            .size(fs.small)
                        )
                        .on_press(FileBrowserMessage::CreateDiskTypeChanged(
                            crate::ftp_ops::DiskCreateType::D64
                        ))
                        .padding([3, 8])
                        .style(
                            if self.create_disk_type == crate::ftp_ops::DiskCreateType::D64 {
                                crate::styles::action_button
                            } else {
                                crate::styles::nav_button
                            }
                        ),
                        button(
                            text(
                                if self.create_disk_type == crate::ftp_ops::DiskCreateType::D71 {
                                    "* D71"
                                } else {
                                    "  D71"
                                }
                            )
                            .size(fs.small)
                        )
                        .on_press(FileBrowserMessage::CreateDiskTypeChanged(
                            crate::ftp_ops::DiskCreateType::D71
                        ))
                        .padding([3, 8])
                        .style(
                            if self.create_disk_type == crate::ftp_ops::DiskCreateType::D71 {
                                crate::styles::action_button
                            } else {
                                crate::styles::nav_button
                            }
                        ),
                        button(
                            text(
                                if self.create_disk_type == crate::ftp_ops::DiskCreateType::D81 {
                                    "* D81"
                                } else {
                                    "  D81"
                                }
                            )
                            .size(fs.small)
                        )
                        .on_press(FileBrowserMessage::CreateDiskTypeChanged(
                            crate::ftp_ops::DiskCreateType::D81
                        ))
                        .padding([3, 8])
                        .style(
                            if self.create_disk_type == crate::ftp_ops::DiskCreateType::D81 {
                                crate::styles::action_button
                            } else {
                                crate::styles::nav_button
                            }
                        ),
                        text(format!("({})", self.create_disk_type))
                            .size(fs.small)
                            .color(dim),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    row![
                        text("Name:")
                            .size(fs.normal)
                            .color(dim)
                            .width(Length::Fixed(80.0)),
                        iced::widget::text_input("DISK NAME", &self.create_disk_name)
                            .on_input(FileBrowserMessage::CreateDiskNameChanged)
                            .padding(6)
                            .size(fs.small as f32)
                            .width(Length::Fixed(200.0)),
                        text(format!("{}/16 chars", self.create_disk_name.len()))
                            .size(fs.small)
                            .color(dim),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    row![
                        text("ID:")
                            .size(fs.normal)
                            .color(dim)
                            .width(Length::Fixed(80.0)),
                        iced::widget::text_input("01 2A", &self.create_disk_id)
                            .on_input(FileBrowserMessage::CreateDiskIdChanged)
                            .padding(6)
                            .size(fs.small as f32)
                            .width(Length::Fixed(100.0)),
                        text("2-char ID + DOS type").size(fs.small).color(dim),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    row![
                        text("File:")
                            .size(fs.normal)
                            .color(dim)
                            .width(Length::Fixed(80.0)),
                        text(preview_filename).size(fs.normal),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    row![
                        Space::new().width(Length::Fill),
                        button(text("Create").size(fs.small))
                            .on_press(FileBrowserMessage::CreateDiskConfirm)
                            .padding([6, 20])
                            .style(crate::styles::action_button),
                    ],
                ]
                .spacing(10)
                .padding(15),
            )
            .style(crate::styles::section_style)
            .width(Length::Fill);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        } else {
            // Filter files based on filter text
            let filtered_files: Vec<&FileEntry> = self
                .files
                .iter()
                .filter(|f| {
                    self.filter.is_empty()
                        || f.name.to_lowercase().contains(&self.filter.to_lowercase())
                })
                .collect();

            // File list with row dividers
            let mut file_list: Vec<Element<'_, FileBrowserMessage>> = Vec::new();
            for (i, entry) in filtered_files.iter().enumerate() {
                if i > 0 {
                    // Add divider between rows
                    file_list.push(rule::horizontal(1).into());
                }
                file_list.push(self.view_file_entry(*entry, font_size));
            }

            let scrollable_list = scrollable(
                Column::with_children(file_list)
                    .spacing(0)
                    .padding(iced::Padding::ZERO.right(12)), // Right padding for scrollbar clearance
            )
            .id(WidgetId::new(FILE_LIST_SCROLLABLE_ID))
            .on_scroll(FileBrowserMessage::Scrolled)
            .height(Length::Fill);

            column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                self.build_column_headers(font_size),
                rule::horizontal(1),
                scrollable_list,
                rule::horizontal(1),
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .into()
        }
    }

    fn view_disk_info_popup(
        &self,
        disk_info: &DiskInfo,
        font_size: u32,
    ) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        // Header with disk name and close button
        let header = row![
            text(format!("{} - ", disk_info.kind)).size(fs.small),
            text(format!("\"{}\"", disk_info.name)).size(fs.normal),
            Space::new().width(Length::Fill),
            text(format!("{} {}", disk_info.disk_id, disk_info.dos_type)).size(fs.small),
            Space::new().width(10),
            tooltip(
                button(text("Close").size(fs.small))
                    .on_press(FileBrowserMessage::CloseDiskInfo)
                    .padding([4, 10])
                    .style(button::secondary),
                "Close directory listing",
                tooltip::Position::Left,
            )
            .style(crate::styles::subtle_tooltip),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Show rendered C64-style PETSCII image if available,
        // otherwise fall back to plain text listing
        let listing: Element<'_, FileBrowserMessage> =
            if let Some(png_bytes) = &self.disk_listing_image {
                // Render as a pixel-perfect C64 screen image
                let handle = iced::widget::image::Handle::from_bytes(png_bytes.clone());
                scrollable(
                    container(
                        iced::widget::image(handle)
                            .width(Length::Fill)
                            .height(Length::Shrink),
                    )
                    .padding(4),
                )
                .height(Length::Fill)
                .into()
            } else {
                // Fallback: plain text listing (used while image is loading)
                let mut items: Vec<Element<'_, FileBrowserMessage>> = Vec::new();
                for entry in &disk_info.entries {
                    let type_color = match entry.file_type {
                        FileType::Prg => iced::Color::from_rgb(0.5, 0.8, 0.5),
                        FileType::Seq => iced::Color::from_rgb(0.5, 0.5, 0.8),
                        FileType::Rel => iced::Color::from_rgb(0.8, 0.8, 0.5),
                        _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
                    };
                    let lock_indicator = if entry.locked { " <" } else { "" };
                    let closed_indicator = if !entry.closed { "*" } else { "" };
                    items.push(
                        row![
                            text(format!("{:>4}", entry.size_blocks))
                                .size(fs.tiny)
                                .width(Length::Fixed(35.0)),
                            text(format!("\"{}\"", entry.name))
                                .size(fs.tiny)
                                .width(Length::Fill),
                            text(format!(
                                "{}{}{}",
                                closed_indicator, entry.file_type, lock_indicator
                            ))
                            .size(fs.tiny)
                            .color(type_color),
                        ]
                        .spacing(5)
                        .align_y(iced::Alignment::Center)
                        .into(),
                    );
                }
                scrollable(
                    Column::with_children(items)
                        .spacing(2)
                        .padding(iced::Padding::ZERO.right(12)),
                )
                .height(Length::Fill)
                .into()
            };

        // Footer with blocks free
        let footer = row![
            text(format!("{} BLOCKS FREE", disk_info.blocks_free)).size(fs.small),
            Space::new().width(Length::Fill),
            text(format!("{} files", disk_info.entries.len())).size(fs.tiny),
        ]
        .spacing(10);

        // Popup container with border styling
        container(
            column![
                header,
                rule::horizontal(1),
                listing,
                rule::horizontal(1),
                footer,
            ]
            .spacing(5)
            .padding(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(crate::styles::subtle_tooltip)
        .into()
    }

    fn view_content_preview_popup<'a>(
        &'a self,
        content: &'a ContentPreview,
        font_size: u32,
    ) -> Element<'a, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        match content {
            ContentPreview::Text {
                filename,
                content,
                line_count,
            } => {
                // Truncate long filenames
                let display_name = if filename.len() > 40 {
                    format!("{}...", &filename[..37])
                } else {
                    filename.clone()
                };

                // Header with filename and close button
                let header = row![
                    text("TEXT - ").size(fs.small),
                    text(display_name.clone()).size(fs.normal),
                    Space::new().width(Length::Fill),
                    text(format!("{} lines", line_count)).size(fs.small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(fs.small))
                            .on_press(FileBrowserMessage::CloseContentPreview)
                            .padding([4, 10])
                            .style(button::secondary),
                        "Close text preview",
                        tooltip::Position::Left,
                    )
                    .style(crate::styles::subtle_tooltip),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                // Text content with line numbers
                let mut text_lines: Vec<Element<'_, FileBrowserMessage>> = Vec::new();
                for (i, line) in content.lines().enumerate() {
                    let line_row = row![
                        text(format!("{:>4}", i + 1))
                            .size(fs.tiny)
                            .width(Length::Fixed(35.0))
                            .color(iced::Color::from_rgb(0.5, 0.5, 0.5)),
                        text(line).size(fs.tiny),
                    ]
                    .spacing(10);
                    text_lines.push(line_row.into());
                }

                // Scrollable text content
                let text_content = scrollable(
                    Column::with_children(text_lines)
                        .spacing(2)
                        .padding(iced::Padding::ZERO.right(12)),
                )
                .height(Length::Fill);

                // Popup container
                container(
                    column![header, rule::horizontal(1), text_content,]
                        .spacing(5)
                        .padding(10),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .style(crate::styles::subtle_tooltip)
                .into()
            }
            ContentPreview::Image {
                filename,
                data,
                width,
                height,
            } => {
                // Truncate long filenames
                let display_name = if filename.len() > 40 {
                    format!("{}...", &filename[..37])
                } else {
                    filename.clone()
                };

                // Header with filename and close button
                let header = row![
                    text("IMAGE - ").size(fs.small),
                    text(display_name.clone()).size(fs.normal),
                    Space::new().width(Length::Fill),
                    text(format!("{}x{}", width, height)).size(fs.small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(fs.small))
                            .on_press(FileBrowserMessage::CloseContentPreview)
                            .padding([4, 10])
                            .style(button::secondary),
                        "Close image preview",
                        tooltip::Position::Left,
                    )
                    .style(crate::styles::subtle_tooltip),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                // Image display using iced's image widget
                let image_handle = iced::widget::image::Handle::from_bytes(data.clone());
                let image_widget = iced::widget::image(image_handle)
                    .width(Length::Fill)
                    .height(Length::Fill);

                // Popup container
                container(
                    column![
                        header,
                        rule::horizontal(1),
                        container(image_widget)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .center_x(Length::Fill)
                            .center_y(Length::Fill),
                    ]
                    .spacing(5)
                    .padding(10),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .style(crate::styles::subtle_tooltip)
                .into()
            }
        }
    }

    fn view_file_entry(
        &self,
        entry: &FileEntry,
        font_size: u32,
    ) -> Element<'_, FileBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let is_checked = self.checked_files.contains(&entry.path);
        // Highlight the row that was last entered (coming back via NavigateUp) or currently selected
        let is_last_visited = self
            .last_entered_child
            .as_ref()
            .map(|p| *p == entry.path)
            .unwrap_or(false);
        let is_selected = self
            .selected_file
            .as_ref()
            .map(|p| *p == entry.path)
            .unwrap_or(false);

        // File type label
        let type_label = if entry.is_dir {
            ""
        } else {
            match entry.extension.as_deref() {
                Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71")
                | Some("g81") => "DSK",
                Some("prg") | Some("p00") | Some("seq") | Some("usr") | Some("rel") => "PRG",
                Some("crt") => "CRT",
                Some("sid") => "SID",
                Some("mod") | Some("xm") | Some("s3m") => "MOD",
                Some("tap") | Some("t64") => "TAP",
                Some("reu") => "REU",
                Some("rom") | Some("bin") => "ROM",
                Some("cfg") => "CFG",
                Some("u2l") | Some("u2p") | Some("u2r") | Some("u64") | Some("ue2") => "UPD",
                Some("txt") | Some("atxt") | Some("nfo") | Some("diz") => "TXT",
                Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") => "IMG",
                Some("pdf") => "PDF",
                Some("zip") => "ZIP",
                _ => "",
            }
        };

        // Truncate long filenames
        let max_name_len = 45;
        let display_name = if entry.name.len() > max_name_len {
            format!("{}...", &entry.name[..max_name_len - 3])
        } else {
            entry.name.clone()
        };

        // Check if this is a disk image that can show info
        let is_disk_image = entry
            .extension
            .as_deref()
            .map_or(false, |ext| crate::file_types::is_disk_image(ext));

        // Check if this is a previewable text or image file
        let is_text_file = dir_preview::is_text_file(&entry.path);
        let is_image_file = dir_preview::is_image_file(&entry.path);
        let is_pdf_file = entry.extension.as_deref() == Some("pdf");

        // Action button based on file type (directories open via filename click)
        let action_button: Element<'_, FileBrowserMessage> = if entry.is_dir {
            Space::new().width(0).into()
        } else {
            match entry.extension.as_deref() {
                Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71")
                | Some("g81") => {
                    let drive = match self.selected_drive {
                        DriveOption::A => "A",
                        DriveOption::B => "B",
                    };
                    let drive_num = match self.selected_drive {
                        DriveOption::A => "8",
                        DriveOption::B => "9",
                    };

                    // Build row with optional info button for D64/D71
                    let mut buttons = row![].spacing(2);

                    // Info button for D64/D71 only
                    if is_disk_image {
                        buttons = buttons.push(
                            tooltip(
                                button(text("?").size(fs.small))
                                    .on_press(FileBrowserMessage::ShowDiskInfo(entry.path.clone()))
                                    .padding([2, 5])
                                    .style(crate::styles::action_button),
                                "Show disk directory listing",
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        );
                    }

                    buttons = buttons
                        .push(
                            tooltip(
                                button(text("Run").size(fs.small))
                                    .on_press(FileBrowserMessage::RunDisk(
                                        entry.path.clone(),
                                        self.selected_drive.to_drive_string(),
                                    ))
                                    .padding([2, 5])
                                    .style(crate::styles::action_button),
                                text(format!("Mount, reset and LOAD\"*\",{},1 + RUN", drive_num))
                                    .size(fs.normal),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        )
                        .push(
                            tooltip(
                                button(text(format!("{}:RW", drive)).size(fs.small))
                                    .on_press(FileBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        self.selected_drive.to_drive_string(),
                                        MountMode::ReadWrite,
                                    ))
                                    .padding([2, 5])
                                    .style(crate::styles::action_button),
                                text(format!("Mount as Drive {} (Read/Write)", drive_num))
                                    .size(fs.normal),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        )
                        .push(
                            tooltip(
                                button(text(format!("{}:RO", drive)).size(fs.small))
                                    .on_press(FileBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        self.selected_drive.to_drive_string(),
                                        MountMode::ReadOnly,
                                    ))
                                    .padding([2, 5])
                                    .style(crate::styles::action_button),
                                text(format!("Mount as Drive {} (Read Only)", drive_num))
                                    .size(fs.normal),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        );

                    buttons.into()
                }
                Some("prg") | Some("crt") => tooltip(
                    button(text("Run").size(fs.small))
                        .on_press(FileBrowserMessage::LoadAndRun(entry.path.clone()))
                        .padding([2, 10])
                        .style(crate::styles::action_button),
                    "Load and run on Ultimate64",
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip)
                .into(),
                Some("sid") => tooltip(
                    button(text("Play").size(fs.small))
                        .on_press(FileBrowserMessage::PlaySid(entry.path.clone()))
                        .padding([2, 8])
                        .style(crate::styles::action_button),
                    "Play SID music on Ultimate64",
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip)
                .into(),
                Some("mod") => tooltip(
                    button(text("Play").size(fs.small))
                        .on_press(FileBrowserMessage::PlayMod(entry.path.clone()))
                        .padding([2, 8])
                        .style(crate::styles::action_button),
                    "Play MOD music on Ultimate64",
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip)
                .into(),
                Some("zip") => {
                    // Extract the ZIP into a sibling subdirectory, then navigate there.
                    // Very large ZIPs (TOSEC etc.) are rejected with a clear error message.
                    tooltip(
                        button(text("Extract").size(fs.small))
                            .on_press(FileBrowserMessage::ExtractZip(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        text(format!(
                            "Extract ZIP to subfolder (max {} MB)",
                            MAX_ZIP_EXTRACT_BYTES / (1024 * 1024)
                        ))
                        .size(fs.normal),
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                }
                _ => {
                    // Check for text, image, or PDF preview
                    if is_text_file {
                        tooltip(
                            button(text("View").size(fs.small))
                                .on_press(FileBrowserMessage::ShowContentPreview(
                                    entry.path.clone(),
                                ))
                                .padding([2, 8])
                                .style(crate::styles::action_button),
                            "View text content",
                            tooltip::Position::Top,
                        )
                        .style(crate::styles::subtle_tooltip)
                        .into()
                    } else if is_image_file {
                        tooltip(
                            button(text("View").size(fs.small))
                                .on_press(FileBrowserMessage::ShowContentPreview(
                                    entry.path.clone(),
                                ))
                                .padding([2, 8])
                                .style(crate::styles::action_button),
                            "View image",
                            tooltip::Position::Top,
                        )
                        .style(crate::styles::subtle_tooltip)
                        .into()
                    } else if is_pdf_file {
                        tooltip(
                            button(text("View").size(fs.small))
                                .on_press(FileBrowserMessage::ShowContentPreview(
                                    entry.path.clone(),
                                ))
                                .padding([2, 8])
                                .style(crate::styles::action_button),
                            "View PDF",
                            tooltip::Position::Top,
                        )
                        .style(crate::styles::subtle_tooltip)
                        .into()
                    } else {
                        Space::new().width(0).into()
                    }
                }
            }
        };

        // Build the row: [checkbox] [name...] [type] [action]
        let path_clone = entry.path.clone();
        let checkbox_element: Element<'_, FileBrowserMessage> = checkbox(is_checked)
            .on_toggle(move |checked| {
                FileBrowserMessage::ToggleFileCheck(path_clone.clone(), checked)
            })
            .size(fs.large)
            .into();

        // Wrap filename in tooltip if truncated to show full name
        let filename_button = button(text(display_name.clone()).size(fs.normal))
            .on_press(FileBrowserMessage::FileSelected(entry.path.clone()))
            .padding([4, 6])
            .width(Length::Fill)
            .style(button::text);

        let filename_element: Element<'_, FileBrowserMessage> = if entry.name.len() > max_name_len {
            tooltip(
                filename_button,
                text(entry.name.clone()).size(fs.normal),
                tooltip::Position::Top,
            )
            .style(crate::styles::subtle_tooltip)
            .into()
        } else {
            filename_button.into()
        };

        // Size column
        let size_text = if entry.is_dir {
            "<DIR>".to_string()
        } else {
            crate::file_types::format_file_size(entry.size.unwrap_or(0))
        };

        let file_row = row![
            // Checkbox (only for files, not dirs)
            checkbox_element,
            // Clickable filename (truncated, with tooltip if needed)
            filename_element,
            // Size column
            text(size_text)
                .size(fs.tiny)
                .width(Length::Fixed(65.0))
                .align_x(iced::alignment::Horizontal::Right),
            // Type label
            text(type_label).size(fs.tiny).width(Length::Fixed(35.0)),
            // Action button
            action_button,
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .padding([2, 4]);

        // Highlight the previously-visited child directory or currently selected file
        // using a subtle coloured background (reverse video feel)
        if is_last_visited || is_selected {
            container(file_row)
                .width(Length::Fill)
                .style(|_theme| container::Style {
                    background: Some(iced::Background::Color(iced::Color::from_rgba(
                        0.45, 0.52, 0.85, 0.25,
                    ))),
                    border: iced::Border {
                        color: iced::Color::from_rgba(0.45, 0.52, 0.85, 0.6),
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..Default::default()
                })
                .into()
        } else {
            file_row.into()
        }
    }

    /// Execute a pending mount/run action directly (drive already confirmed enabled).
    fn dispatch_action(
        &mut self,
        action: PendingDriveAction,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<FileBrowserMessage> {
        match action {
            PendingDriveAction::Mount(path, drive, mode) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Mounting {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Task::perform(
                        mount_disk_async(conn, path, drive, mode),
                        FileBrowserMessage::MountCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }
            PendingDriveAction::Run(path, drive) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Running {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Task::perform(
                        run_disk_async(conn, path, drive),
                        FileBrowserMessage::RunDiskCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }
        }
    }

    fn sort_files(&mut self) {
        use crate::file_types::{SortColumn, SortOrder};
        let col = self.sort_column;
        let order = self.sort_order;
        self.files.sort_by(|a, b| {
            // Directories always come first
            match (a.is_dir, b.is_dir) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
            let cmp = match col {
                SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortColumn::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
                SortColumn::Type => {
                    let a_ext = a.extension.as_deref().unwrap_or("");
                    let b_ext = b.extension.as_deref().unwrap_or("");
                    a_ext.cmp(b_ext)
                }
            };
            if order == SortOrder::Descending {
                cmp.reverse()
            } else {
                cmp
            }
        });
    }

    fn load_directory(&mut self, path: &Path) {
        self.files.clear();
        self.filter.clear();

        if let Ok(entries) = std::fs::read_dir(path) {
            let files: Vec<FileEntry> = entries
                .filter_map(|entry| {
                    entry.ok().and_then(|e| {
                        let path = e.path();
                        let name = e.file_name().to_string_lossy().to_string();

                        // Skip hidden files on Unix
                        if name.starts_with('.') {
                            return None;
                        }

                        let is_dir = path.is_dir();
                        let metadata = e.metadata().ok();
                        let size = metadata.as_ref().map(|m| m.len());

                        let extension = if !is_dir {
                            path.extension()
                                .and_then(|ext| ext.to_str())
                                .map(|s| s.to_lowercase())
                        } else {
                            None
                        };

                        Some(FileEntry {
                            path,
                            name,
                            is_dir,
                            extension,
                            size,
                        })
                    })
                })
                .collect();

            self.files = files;
            self.sort_files();
        }

        log::debug!("Loaded {} items from {}", self.files.len(), path.display());
    }
}

async fn load_disk_info_async(path: PathBuf) -> Result<DiskInfo, String> {
    // Run disk reading in blocking task to avoid blocking async runtime
    tokio::task::spawn_blocking(move || disk_image::read_disk_info(&path))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

/// Read a ZIP from disk, extract it to `target_dir`, and return the target dir
/// path on success.  The actual extraction runs on a blocking thread so the
/// async runtime is not stalled.
async fn extract_zip_file_async(zip_path: PathBuf, target_dir: PathBuf) -> Result<PathBuf, String> {
    // Read the ZIP bytes from disk
    let data = tokio::fs::read(&zip_path)
        .await
        .map_err(|e| format!("Failed to read ZIP file: {}", e))?;

    let filename = zip_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("archive.zip")
        .to_string();

    let target_dir_clone = target_dir.clone();
    // Run the CPU-bound extraction off the async thread pool
    tokio::task::spawn_blocking(move || {
        extract_zip_to_dir(&data, &filename, &target_dir_clone)
            .map(|_| target_dir_clone)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

async fn load_content_preview_async(path: PathBuf) -> Result<ContentPreview, String> {
    // Determine if text or image based on extension
    if dir_preview::is_text_file(&path) {
        dir_preview::load_text_file_async(path).await
    } else if dir_preview::is_image_file(&path) {
        dir_preview::load_image_file_async(path).await
    } else if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        == Some("pdf".to_string())
    {
        pdf_preview::load_pdf_preview_async(path).await
    } else {
        Err("Unsupported file type for preview".to_string())
    }
}

async fn mount_disk_async(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    drive: String,
    mode: MountMode,
) -> Result<(), String> {
    log::info!(
        "Mounting {} to drive {} ({:?})",
        path.display(),
        drive,
        mode
    );

    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            conn.mount_disk_image(&path, drive.clone(), mode, false)
                .map_err(|e| {
                    log::error!("Mount error: {}", e);
                    e.to_string()
                })
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => {
            if inner.is_ok() {
                log::info!("Mount successful");
            }
            inner
        }
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Mount timed out - device may be offline".to_string()),
    }
}

async fn run_disk_async(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    drive: String,
) -> Result<(), String> {
    log::info!("Running disk {} on drive {}", path.display(), drive);

    // Determine device number based on drive
    let device_num = if drive == "a" { "8" } else { "9" };

    // Use longer timeout because this operation includes boot delays (~8.5s of sleeps)
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(RUN_DISK_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();

            // 1. Mount the disk image (read-only is fine for running)
            conn.mount_disk_image(&path, drive.clone(), MountMode::ReadOnly, false)
                .map_err(|e| format!("Mount failed: {}", e))?;

            // Small delay to ensure mount completes
            std::thread::sleep(std::time::Duration::from_millis(500));

            // 2. Reset the machine
            conn.reset().map_err(|e| format!("Reset failed: {}", e))?;

            // Wait for C64 to boot up
            std::thread::sleep(std::time::Duration::from_secs(3));

            // 3. Type LOAD"*",8,1 (or 9) and RUN
            let load_cmd = format!("load \"*\",{},1\n", device_num);
            conn.type_text(&load_cmd)
                .map_err(|e| format!("Type LOAD failed: {}", e))?;

            // Wait for program to load
            std::thread::sleep(std::time::Duration::from_secs(5));

            // 4. Type RUN
            conn.type_text("run\n")
                .map_err(|e| format!("Type RUN failed: {}", e))?;

            Ok(())
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Run disk timed out - device may be offline".to_string()),
    }
}

async fn load_and_run_async(connection: Arc<Mutex<Rest>>, path: PathBuf) -> Result<(), String> {
    log::info!("Loading and running: {}", path.display());

    let data = std::fs::read(&path).map_err(|e| {
        log::error!("Failed to read file: {}", e);
        e.to_string()
    })?;

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            match ext.as_deref() {
                Some("crt") => {
                    log::info!("Running as CRT cartridge");
                    conn.run_crt(&data).map_err(|e| e.to_string())
                }
                Some("prg") => {
                    log::info!("Running as PRG");
                    conn.run_prg(&data).map_err(|e| e.to_string())
                }
                _ => Err("Unsupported file type".to_string()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Load timed out - device may be offline".to_string()),
    }
}

/// Outcome of the one-shot CSDB → Assembly64 folder migration.
#[derive(Debug, Clone, Copy)]
pub enum MigrationOutcome {
    /// No legacy folder found — nothing to do (clean install or already migrated).
    Nothing,
    /// `CSDB/` was renamed to `Assembly64/`.
    Renamed,
    /// Both folders existed; we left them alone to avoid overwriting files.
    BothExisted,
    /// `rename` failed at the filesystem level.
    Failed(std::io::ErrorKind),
}

/// One-shot migration of the legacy `CSDB/` downloads folder to
/// `Assembly64/` so users upgrading from the old browser keep their prior
/// downloads under the new toolbar shortcut.
///
/// Tries common case variants (`CSDB`, `csdb`, `Csdb`, `CSDb`) since macOS
/// filesystems are typically case-insensitive but case-preserving — the
/// stored name depends on whichever variant created the folder originally.
/// Returns the [`MigrationOutcome`] so callers can surface it.
pub fn migrate_csdb_to_assembly(base: &std::path::Path) -> MigrationOutcome {
    let new = base.join("Assembly64");
    let mut found_old: Option<std::path::PathBuf> = None;
    for variant in ["CSDB", "csdb", "Csdb", "CSDb"] {
        let candidate = base.join(variant);
        if candidate.exists() {
            found_old = Some(candidate);
            break;
        }
    }
    let Some(old) = found_old else {
        return MigrationOutcome::Nothing;
    };
    if new.exists() {
        log::info!(
            "Both {} and Assembly64 exist — skipping migration (manually merge if needed)",
            old.display()
        );
        return MigrationOutcome::BothExisted;
    }
    match std::fs::rename(&old, &new) {
        Ok(()) => {
            log::info!("Migrated {} → {}", old.display(), new.display());
            MigrationOutcome::Renamed
        }
        Err(e) => {
            log::warn!("Could not migrate {} → Assembly64: {}", old.display(), e);
            MigrationOutcome::Failed(e.kind())
        }
    }
}

/// Check whether the given drive (\"a\" or \"b\") is currently enabled.
/// Returns Ok(true) if enabled, Ok(false) if disabled, Err if unreachable.
pub async fn check_drive_enabled_async(
    host: String,
    drive: String,
    password: Option<String>,
) -> Result<bool, String> {
    let category = if drive == "a" {
        "Drive%20A%20Settings"
    } else {
        "Drive%20B%20Settings"
    };

    let url = format!("http://{}/v1/configs/{}/Drive", host, category);
    let client =
        crate::net_utils::build_device_client(REST_TIMEOUT_SECS).map_err(|e| e.to_string())?;

    let req = crate::net_utils::with_password(client.get(&url), password.as_deref());

    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    // Response: {"Drive B Settings":{"Drive":{"current":"Disabled",...}},"errors":[]}
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let category_key = if drive == "a" {
        "Drive A Settings"
    } else {
        "Drive B Settings"
    };
    let current = json[category_key]["Drive"]["current"]
        .as_str()
        .unwrap_or("Disabled");
    log::info!("Drive {} config current value: {}", drive, current);
    Ok(current == "Enabled")
}

/// Temporarily enable a drive via the config API without writing to flash.
/// Uses POST /v1/configs with the drive-specific enable key.
pub async fn enable_drive_async(
    host: String,
    drive: String, // "a" or "b"
    password: Option<String>,
) -> Result<(), String> {
    let category = if drive == "a" {
        "Drive A Settings"
    } else {
        "Drive B Settings"
    };

    let url = format!("http://{}/v1/configs", host);
    let body = serde_json::json!({
        category: { "Drive": "Enabled" }
    });

    let client =
        crate::net_utils::build_device_client(REST_TIMEOUT_SECS).map_err(|e| e.to_string())?;

    let req = crate::net_utils::with_password(
        client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body),
        password.as_deref(),
    );

    let resp = req.send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        log::info!(
            "Drive {} enabled temporarily via config API",
            drive.to_uppercase()
        );
        Ok(())
    } else {
        Err(format!("Config API returned HTTP {}", resp.status()))
    }
}
