use iced::{
    Element, Length, Subscription, Task,
    widget::{
        Column, Space, button, checkbox, column, container, progress_bar, row, rule, scrollable,
        text, text_input, tooltip,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use ultimate64::Rest;
use walkdir::WalkDir;

use crate::api;
use crate::dir_preview::ContentPreview;
use crate::disk_image::{self, DiskInfo, FileType};

/// Disk format chosen in the create-disk dialog
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskCreateType {
    D64,
    D71,
    D81,
}

impl std::fmt::Display for DiskCreateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiskCreateType::D64 => write!(f, "D64  (1541 · 174 KB)"),
            DiskCreateType::D71 => write!(f, "D71  (1571 · 349 KB)"),
            DiskCreateType::D81 => write!(f, "D81  (1581 · 800 KB)"),
        }
    }
}

/// Timeout for FTP operations to prevent hangs when device goes offline
const FTP_TIMEOUT_SECS: u64 = 15;
/// Longer timeout for directory uploads which may take time
const FTP_UPLOAD_DIR_TIMEOUT_SECS: u64 = 120;
/// Longer timeout for content preview downloads (PDFs can be large)
const FTP_PREVIEW_TIMEOUT_SECS: u64 = 60;

/// Shared progress state between async FTP tasks and the UI.
/// Updated by blocking tasks, polled by iced subscription every 250ms.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: String,
    pub operation: String, // "Downloading", "Uploading", etc.
    pub done: bool,
}

#[derive(Debug, Clone)]
pub enum RemoteBrowserMessage {
    RefreshFiles,
    FilesLoaded(Result<Vec<RemoteFileEntry>, String>),
    FileSelected(String),
    NavigateUp,
    NavigateToPath(String),
    DownloadFile(String),
    DownloadComplete(Result<(String, Vec<u8>), String>),
    UploadFile(PathBuf, String), // local path, remote destination
    UploadComplete(Result<String, String>),
    UploadDirectory(PathBuf, String), // local directory path, remote destination
    UploadDirectoryComplete(Result<String, String>),
    // Runners - execute files on Ultimate64
    RunPrg(String),
    RunCrt(String),
    PlaySid(String),
    PlayMod(String),
    RunnerComplete(Result<String, String>),
    // Disk mounting
    MountDisk(String, String, String), // path, drive (a/b), mode (readwrite/readonly/unlinked)
    MountComplete(Result<String, String>),
    RunDisk(String, String), // path, drive - mount and reset
    // Selection (checkboxes)
    ToggleFileCheck(String, bool),
    SelectAll,
    SelectNone,
    // Batch download
    DownloadCheckedToLocal(PathBuf), // local destination directory
    DownloadBatchComplete(Result<String, String>),
    // Filter
    FilterChanged(String),
    // Disk info popup
    ShowDiskInfo(String),
    DiskInfoLoaded(Result<DiskInfo, String>),

    // Disk image creator
    ShowCreateDisk,
    CloseCreateDisk,
    CreateDiskNameChanged(String),
    CreateDiskIdChanged(String),
    CreateDiskTypeChanged(DiskCreateType),
    CreateDiskConfirm,
    CreateDiskComplete(Result<String, String>),
    CloseDiskInfo,
    // Content preview popup (text/image files)
    ShowContentPreview(String),
    ContentPreviewLoaded(Result<ContentPreview, String>),
    CloseContentPreview,
    // Transfer progress (polled by subscription)
    ProgressTick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct RemoteBrowser {
    pub current_path: String,
    pub files: Vec<RemoteFileEntry>,
    pub selected_file: Option<String>,
    pub status_message: Option<String>,
    pub is_loading: bool,
    pub is_connected: bool,
    pub host_address: Option<String>,
    pub password: Option<String>,
    pub filter: String,
    // Checked files for batch operations
    pub checked_files: HashSet<String>,
    // Disk info popup state
    disk_info_popup: Option<DiskInfo>,
    disk_info_path: Option<String>,
    disk_info_loading: bool,
    // Rendered C64-style PETSCII listing image (PNG bytes)
    disk_listing_image: Option<Vec<u8>>,
    // Disk image creator dialog state
    show_create_disk: bool,
    create_disk_name: String,
    create_disk_id: String,
    create_disk_type: DiskCreateType,
    create_disk_busy: bool,

    // Content preview popup state (text/image files)
    content_preview: Option<ContentPreview>,
    content_preview_path: Option<String>,
    content_preview_loading: bool,
    // Transfer progress (shared with async FTP tasks)
    transfer_progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
}

impl Default for RemoteBrowser {
    fn default() -> Self {
        Self {
            current_path: "/".to_string(),
            files: Vec::new(),
            selected_file: None,
            status_message: Some("Not connected".to_string()),
            is_loading: false,
            is_connected: false,
            host_address: None,
            password: None,
            filter: String::new(),
            checked_files: HashSet::new(),
            disk_info_popup: None,
            disk_info_path: None,
            show_create_disk: false,
            create_disk_name: "NEWDISK".to_string(),
            create_disk_id: "01 2A".to_string(),
            create_disk_type: DiskCreateType::D64,
            create_disk_busy: false,
            disk_info_loading: false,
            disk_listing_image: None,
            content_preview: None,
            content_preview_path: None,
            content_preview_loading: false,
            transfer_progress: Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl RemoteBrowser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_host(&mut self, host: Option<String>, password: Option<String>) {
        // Strip http:// prefix if present, we just need the IP
        self.host_address = host.map(|h| {
            h.trim_start_matches("http://")
                .trim_start_matches("https://")
                .to_string()
        });
        self.password = password;
        self.is_connected = self.host_address.is_some();
        if self.host_address.is_none() {
            self.files.clear();
            self.status_message = Some("Not connected".to_string());
        }
    }

    pub fn update(
        &mut self,
        message: RemoteBrowserMessage,
        _connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<RemoteBrowserMessage> {
        match message {
            RemoteBrowserMessage::RefreshFiles => {
                if let Some(host) = &self.host_address {
                    self.is_loading = true;
                    self.status_message = Some("Loading...".to_string());
                    let path = self.current_path.clone();
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        fetch_files_ftp(host, path, password),
                        RemoteBrowserMessage::FilesLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    self.is_connected = false;
                    Task::none()
                }
            }

            RemoteBrowserMessage::FilesLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(files) => {
                        self.files = files;
                        self.is_connected = true;
                        self.status_message = Some(format!("{} items", self.files.len()));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("{}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::FileSelected(path) => {
                // Check if it's a directory
                if let Some(entry) = self.files.iter().find(|f| f.path == path) {
                    if entry.is_dir {
                        self.current_path = path;
                        self.selected_file = None;
                        self.checked_files.clear();
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    } else {
                        self.selected_file = Some(path);
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::NavigateUp => {
                if self.current_path != "/" {
                    self.checked_files.clear();
                    if let Some(parent) = PathBuf::from(&self.current_path).parent() {
                        self.current_path = parent.to_string_lossy().to_string();
                        if self.current_path.is_empty() {
                            self.current_path = "/".to_string();
                        }
                    }
                    return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                }
                Task::none()
            }

            RemoteBrowserMessage::NavigateToPath(path) => {
                self.current_path = path;
                self.checked_files.clear();
                self.update(RemoteBrowserMessage::RefreshFiles, _connection)
            }

            RemoteBrowserMessage::DownloadFile(remote_path) => {
                if let Some(host) = &self.host_address {
                    let filename = remote_path.rsplit('/').next().unwrap_or("file").to_string();
                    self.status_message = Some(format!("Downloading {}...", filename));
                    // Set initial progress for single file
                    if let Ok(mut g) = self.transfer_progress.lock() {
                        *g = Some(TransferProgress {
                            current: 0,
                            total: 1,
                            current_file: filename,
                            operation: "Downloading".to_string(),
                            done: false,
                        });
                    }
                    let host = host.clone();
                    let password = self.password.clone();
                    let progress = self.transfer_progress.clone();
                    Task::perform(
                        download_file_ftp_with_progress(host, remote_path, password, progress),
                        RemoteBrowserMessage::DownloadComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::DownloadComplete(result) => {
                // Clear transfer progress
                if let Ok(mut g) = self.transfer_progress.lock() {
                    *g = None;
                }
                match result {
                    Ok((name, _data)) => {
                        self.status_message = Some(format!("Downloaded: {}", name));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Download failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::UploadFile(local_path, remote_dest) => {
                if let Some(host) = &self.host_address {
                    let filename = local_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    self.status_message = Some(format!("Uploading {}...", filename));
                    // Set initial progress for single file upload
                    if let Ok(mut g) = self.transfer_progress.lock() {
                        *g = Some(TransferProgress {
                            current: 0,
                            total: 1,
                            current_file: filename,
                            operation: "Uploading".to_string(),
                            done: false,
                        });
                    }
                    let host = host.clone();
                    let password = self.password.clone();
                    let progress = self.transfer_progress.clone();
                    Task::perform(
                        upload_file_ftp(host, local_path, remote_dest, password, progress),
                        RemoteBrowserMessage::UploadComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::UploadComplete(result) => {
                // Clear transfer progress
                if let Ok(mut g) = self.transfer_progress.lock() {
                    *g = None;
                }
                match result {
                    Ok(name) => {
                        self.status_message = Some(format!("Uploaded: {}", name));
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Upload failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::UploadDirectory(local_path, remote_dest) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Uploading directory...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    let progress = self.transfer_progress.clone();
                    Task::perform(
                        upload_directory_ftp(host, local_path, remote_dest, password, progress),
                        RemoteBrowserMessage::UploadDirectoryComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::UploadDirectoryComplete(result) => {
                // Clear transfer progress
                if let Ok(mut g) = self.transfer_progress.lock() {
                    *g = None;
                }
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Directory upload failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::RunPrg(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Running PRG...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move { api::run_prg(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::RunCrt(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Running CRT...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move { api::run_crt(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::PlaySid(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Playing SID...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move { api::sidplay(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::PlayMod(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Playing MOD...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move { api::modplay(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::RunnerComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::MountDisk(path, drive, mode) => {
                if let Some(host) = &self.host_address {
                    self.status_message =
                        Some(format!("Mounting to drive {}...", drive.to_uppercase()));
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move { api::mount_disk(&host, &path, &drive, &mode, password).await },
                        RemoteBrowserMessage::MountComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::MountComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Mount failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::RunDisk(path, drive) => {
                if let Some(host) = &self.host_address {
                    self.status_message =
                        Some(format!("Running disk on drive {}...", drive.to_uppercase()));
                    let host = host.clone();
                    let password = self.password.clone();
                    let conn = _connection.clone();
                    Task::perform(
                        async move { api::run_disk(&host, &path, &drive, password, conn).await },
                        RemoteBrowserMessage::MountComplete, // Reuse MountComplete for result
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::ToggleFileCheck(path, checked) => {
                if checked {
                    self.checked_files.insert(path);
                } else {
                    self.checked_files.remove(&path);
                }
                Task::none()
            }

            RemoteBrowserMessage::SelectAll => {
                for file in &self.files {
                    self.checked_files.insert(file.path.clone());
                }
                Task::none()
            }

            RemoteBrowserMessage::SelectNone => {
                self.checked_files.clear();
                Task::none()
            }

            RemoteBrowserMessage::DownloadCheckedToLocal(local_dest) => {
                if self.checked_files.is_empty() {
                    self.status_message = Some("No files selected".to_string());
                    return Task::none();
                }
                if let Some(host) = &self.host_address {
                    let checked: Vec<String> = self.checked_files.iter().cloned().collect();
                    // Separate files and directories
                    let mut file_paths = Vec::new();
                    let mut dir_paths = Vec::new();
                    for path in &checked {
                        if let Some(entry) = self.files.iter().find(|f| &f.path == path) {
                            if entry.is_dir {
                                dir_paths.push(path.clone());
                            } else {
                                file_paths.push(path.clone());
                            }
                        }
                    }
                    let total = file_paths.len() + dir_paths.len();
                    self.status_message = Some(format!("Downloading {} item(s)...", total));
                    let host = host.clone();
                    let password = self.password.clone();
                    let progress = self.transfer_progress.clone();
                    Task::perform(
                        download_batch_ftp(
                            host, file_paths, dir_paths, local_dest, password, progress,
                        ),
                        RemoteBrowserMessage::DownloadBatchComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::DownloadBatchComplete(result) => {
                // Clear transfer progress
                if let Ok(mut g) = self.transfer_progress.lock() {
                    *g = None;
                }
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.checked_files.clear();
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Download failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::FilterChanged(value) => {
                self.filter = value;
                Task::none()
            }

            // Disk info popup messages
            RemoteBrowserMessage::ShowDiskInfo(path) => {
                if let Some(host) = &self.host_address {
                    self.disk_info_loading = true;
                    self.disk_info_path = Some(path.clone());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        load_remote_disk_info(host, path, password),
                        RemoteBrowserMessage::DiskInfoLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::DiskInfoLoaded(result) => {
                self.disk_info_loading = false;
                match result {
                    Ok(info) => {
                        // Render a C64-style PETSCII listing image
                        self.disk_listing_image =
                            Some(crate::dir_preview::render_disk_listing_image(&info));
                        self.disk_info_popup = Some(info);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to read disk: {}", e));
                        self.disk_info_path = None;
                    }
                }
                Task::none()
            }

            // ── Disk creator ──────────────────────────────────────────────
            RemoteBrowserMessage::ShowCreateDisk => {
                self.show_create_disk = true;
                Task::none()
            }
            RemoteBrowserMessage::CloseCreateDisk => {
                self.show_create_disk = false;
                self.create_disk_busy = false;
                Task::none()
            }
            RemoteBrowserMessage::CreateDiskNameChanged(s) => {
                // PETSCII disk names are max 16 chars, uppercase only
                self.create_disk_name = s.to_uppercase().chars().take(16).collect();
                Task::none()
            }
            RemoteBrowserMessage::CreateDiskIdChanged(s) => {
                self.create_disk_id = s.to_uppercase().chars().take(5).collect();
                Task::none()
            }
            RemoteBrowserMessage::CreateDiskTypeChanged(t) => {
                self.create_disk_type = t;
                Task::none()
            }
            RemoteBrowserMessage::CreateDiskConfirm => {
                if let Some(host) = self.host_address.clone() {
                    self.create_disk_busy = true;
                    let name = self.create_disk_name.clone();
                    let id = self.create_disk_id.clone();
                    let disk_type = self.create_disk_type;
                    let dest = self.current_path.clone();
                    let password = self.password.clone();
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                create_and_upload_disk(host, name, id, disk_type, dest, password)
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                        },
                        RemoteBrowserMessage::CreateDiskComplete,
                    )
                } else {
                    Task::none()
                }
            }
            RemoteBrowserMessage::CreateDiskComplete(result) => {
                self.create_disk_busy = false;
                self.show_create_disk = false;
                match result {
                    Ok(name) => {
                        self.status_message = Some(format!("Created: {}", name));
                        // Refresh the file list
                        if let Some(host) = self.host_address.clone() {
                            let path = self.current_path.clone();
                            let password = self.password.clone();
                            self.is_loading = true;
                            return Task::perform(
                                fetch_files_ftp(host, path, password),
                                RemoteBrowserMessage::FilesLoaded,
                            );
                        }
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Create failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::CloseDiskInfo => {
                self.disk_info_popup = None;
                self.disk_info_path = None;
                self.disk_listing_image = None;
                Task::none()
            }

            // Content preview popup messages (text/image files)
            RemoteBrowserMessage::ShowContentPreview(path) => {
                if let Some(host) = &self.host_address {
                    self.content_preview_loading = true;
                    let filename = path.rsplit('/').next().unwrap_or("file");
                    self.status_message = Some(format!("Downloading {}...", filename));

                    self.content_preview_path = Some(path.clone());
                    let host = host.clone();
                    let password = self.password.clone();
                    Task::perform(
                        load_remote_content_preview(host, path, password),
                        RemoteBrowserMessage::ContentPreviewLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::ContentPreviewLoaded(result) => {
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

            RemoteBrowserMessage::CloseContentPreview => {
                self.content_preview = None;
                self.content_preview_path = None;
                Task::none()
            }

            RemoteBrowserMessage::ProgressTick => {
                // Read shared progress state and update status message
                if let Ok(guard) = self.transfer_progress.lock() {
                    if let Some(ref progress) = *guard {
                        if progress.done {
                            // Transfer complete — clear progress
                            drop(guard);
                            if let Ok(mut g) = self.transfer_progress.lock() {
                                *g = None;
                            }
                        } else {
                            self.status_message = Some(format!(
                                "{} ({}/{}) {}",
                                progress.operation,
                                progress.current,
                                progress.total,
                                progress.current_file
                            ));
                        }
                    }
                }
                Task::none()
            }
        }
    }

    pub fn view(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        // Path display
        let display_path = if self.current_path.len() > 35 {
            format!("...{}", &self.current_path[self.current_path.len() - 32..])
        } else {
            self.current_path.clone()
        };

        // Navigation buttons with filter
        let checked_count = self.checked_files.len();
        let nav_buttons = row![
            tooltip(
                button(text("⬆ Up").size(normal))
                    .on_press(RemoteBrowserMessage::NavigateUp)
                    .padding([4, 8]),
                "Go to parent folder",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("✔ All").size(tiny))
                    .on_press(RemoteBrowserMessage::SelectAll)
                    .padding([2, 6]),
                "Select all files",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("✖ None").size(tiny))
                    .on_press(RemoteBrowserMessage::SelectNone)
                    .padding([2, 6]),
                "Deselect all files",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            if checked_count > 0 {
                text(format!("{} selected", checked_count)).size(small)
            } else {
                text("").size(small)
            },
            Space::new().width(Length::Fill),
            text("Filter:").size(small),
            text_input("filter...", &self.filter)
                .on_input(RemoteBrowserMessage::FilterChanged)
                .size(normal)
                .padding(4)
                .width(Length::Fixed(100.0)),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Quick navigation to common paths
        let quick_nav = row![
            tooltip(
                button(text("/").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/".to_string()))
                    .padding([2, 6]),
                "Root directory",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Usb0").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/Usb0".to_string()))
                    .padding([2, 6]),
                "USB Drive 0",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Usb1").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/Usb1".to_string()))
                    .padding([2, 6]),
                "USB Drive 1",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Usb2").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/Usb2".to_string()))
                    .padding([2, 6]),
                "USB Drive 2",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("SD").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/SD".to_string()))
                    .padding([2, 6]),
                "SD Card",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            Space::new().width(Length::Fill),
            tooltip(
                {
                    let btn = button(text("💾 New Disk").size(small)).padding([2, 8]);
                    if self.is_connected {
                        btn.on_press(RemoteBrowserMessage::ShowCreateDisk)
                    } else {
                        btn
                    }
                },
                "Create a new blank D64/D71/D81 disk image and upload it",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(3);

        // Path and status
        let path_display = text(display_path).size(normal);

        // ── Create-disk dialog ────────────────────────────────────────────────
        if self.show_create_disk {
            let dialog = self.view_create_disk_dialog(font_size);
            return column![
                nav_buttons,
                quick_nav,
                path_display,
                self.view_status_bar(small),
                dialog,
            ]
            .spacing(5)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // If disk info popup is open, show it instead of the file list
        if let Some(disk_info) = &self.disk_info_popup {
            let popup = self.view_disk_info_popup(disk_info, font_size);

            return column![
                nav_buttons,
                quick_nav,
                path_display,
                self.view_status_bar(small),
                popup,
            ]
            .spacing(5)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }
        // Show loading panel while downloading content for preview
        if self.content_preview_loading {
            let loading_panel = container(
                column![
                    text("Downloading...").size(normal),
                    text(self.content_preview_path.as_deref().unwrap_or("")).size(small),
                ]
                .spacing(10)
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(container::bordered_box);

            return column![
                nav_buttons,
                quick_nav,
                path_display,
                self.view_status_bar(small),
                loading_panel,
            ]
            .spacing(5)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }
        // If content preview popup is open, show it instead of the file list
        if let Some(content_preview) = &self.content_preview {
            let popup = self.view_content_preview_popup(content_preview, font_size);

            return column![
                nav_buttons,
                quick_nav,
                path_display,
                self.view_status_bar(small),
                popup,
            ]
            .spacing(5)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }

        // File list
        let file_list: Element<'_, RemoteBrowserMessage> = if self.files.is_empty() {
            if self.is_loading {
                text("Loading...").size(normal).into()
            } else if !self.is_connected {
                text("Connect to Ultimate64 to browse files")
                    .size(normal)
                    .into()
            } else {
                text("Empty directory").size(normal).into()
            }
        } else {
            // Filter files based on filter text
            let filtered_files: Vec<&RemoteFileEntry> = self
                .files
                .iter()
                .filter(|f| {
                    self.filter.is_empty()
                        || f.name.to_lowercase().contains(&self.filter.to_lowercase())
                })
                .collect();

            let mut items: Vec<Element<'_, RemoteBrowserMessage>> = Vec::new();

            for (i, entry) in filtered_files.iter().enumerate() {
                if i > 0 {
                    // Add divider between rows
                    items.push(rule::horizontal(1).into());
                }

                // File type label
                let type_label = if entry.is_dir {
                    ""
                } else {
                    get_file_icon(&entry.name)
                };

                // Truncate long filenames
                let max_name_len = 45;
                let display_name = if entry.name.len() > max_name_len {
                    format!("{}...", &entry.name[..max_name_len - 3])
                } else {
                    entry.name.clone()
                };

                // Check if this is a disk image that can show info (D64/D71 only)
                let is_disk_image = {
                    let lower = entry.name.to_lowercase();
                    lower.ends_with(".d64") || lower.ends_with(".d71")
                };

                // Check if this is a previewable text or image file
                let is_text_file = is_remote_text_file(&entry.name);
                let is_image_file = is_remote_image_file(&entry.name);
                let is_pdf_file = is_remote_pdf_file(&entry.name);

                // Action button based on file type
                let ext = entry.name.to_lowercase();
                let action_button: Element<'_, RemoteBrowserMessage> = if entry.is_dir {
                    tooltip(
                        button(text("Open").size(small))
                            .on_press(RemoteBrowserMessage::FileSelected(entry.path.clone()))
                            .padding([2, 8]),
                        "Open folder",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if ext.ends_with(".prg") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunPrg(entry.path.clone()))
                            .padding([2, 8]),
                        "Load and run PRG file",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if ext.ends_with(".crt") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunCrt(entry.path.clone()))
                            .padding([2, 8]),
                        "Load cartridge image",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if ext.ends_with(".sid") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlaySid(entry.path.clone()))
                            .padding([2, 8]),
                        "Play SID music",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if ext.ends_with(".mod") || ext.ends_with(".xm") || ext.ends_with(".s3m") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlayMod(entry.path.clone()))
                            .padding([2, 8]),
                        "Play MOD/tracker music",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if ext.ends_with(".d64")
                    || ext.ends_with(".g64")
                    || ext.ends_with(".d71")
                    || ext.ends_with(".g71")
                    || ext.ends_with(".d81")
                {
                    // Disk image - show run and mount buttons
                    let mut buttons = row![].spacing(2);

                    // Info button for D64/D71 only
                    if is_disk_image {
                        buttons = buttons.push(
                            tooltip(
                                button(text("?").size(small))
                                    .on_press(RemoteBrowserMessage::ShowDiskInfo(
                                        entry.path.clone(),
                                    ))
                                    .padding([2, 5]),
                                "Show disk directory listing",
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box),
                        );
                    }

                    buttons = buttons
                        .push(
                            tooltip(
                                button(text("Run").size(tiny))
                                    .on_press(RemoteBrowserMessage::RunDisk(
                                        entry.path.clone(),
                                        "a".to_string(),
                                    ))
                                    .padding([2, 6]),
                                "Mount, reset & LOAD\"*\",8,1",
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box),
                        )
                        .push(
                            tooltip(
                                button(text("A:RW").size(tiny))
                                    .on_press(RemoteBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        "a".to_string(),
                                        "readwrite".to_string(),
                                    ))
                                    .padding([2, 4]),
                                "Mount to Drive A (Read/Write)",
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box),
                        )
                        .push(
                            tooltip(
                                button(text("A:RO").size(tiny))
                                    .on_press(RemoteBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        "a".to_string(),
                                        "readonly".to_string(),
                                    ))
                                    .padding([2, 4]),
                                "Mount to Drive A (Read Only)",
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box),
                        )
                        .push(
                            tooltip(
                                button(text("B:RW").size(tiny))
                                    .on_press(RemoteBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        "b".to_string(),
                                        "readwrite".to_string(),
                                    ))
                                    .padding([2, 4]),
                                "Mount to Drive B (Read/Write)",
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box),
                        );

                    buttons.into()
                } else if is_text_file {
                    tooltip(
                        button(text("View").size(small))
                            .on_press(RemoteBrowserMessage::ShowContentPreview(entry.path.clone()))
                            .padding([2, 8]),
                        "View text content",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if is_image_file {
                    tooltip(
                        button(text("View").size(small))
                            .on_press(RemoteBrowserMessage::ShowContentPreview(entry.path.clone()))
                            .padding([2, 8]),
                        "View image",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else if is_pdf_file {
                    tooltip(
                        button(text("View").size(small))
                            .on_press(RemoteBrowserMessage::ShowContentPreview(entry.path.clone()))
                            .padding([2, 8]),
                        "View PDF",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else {
                    iced::widget::Space::new().width(0).into()
                };

                // Wrap filename in tooltip if truncated to show full name
                let is_truncated = entry.name.len() > max_name_len;
                let filename_button = button(text(display_name.clone()).size(normal))
                    .on_press(RemoteBrowserMessage::FileSelected(entry.path.clone()))
                    .padding([4, 6])
                    .width(Length::Fill)
                    .style(button::text);

                let filename_element: Element<'_, RemoteBrowserMessage> = if is_truncated {
                    tooltip(
                        filename_button,
                        text(&entry.name).size(normal),
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into()
                } else {
                    filename_button.into()
                };

                let is_checked = self.checked_files.contains(&entry.path);
                let path_for_check = entry.path.clone();
                let checkbox_element: Element<'_, RemoteBrowserMessage> = checkbox(is_checked)
                    .on_toggle(move |checked| {
                        RemoteBrowserMessage::ToggleFileCheck(path_for_check.clone(), checked)
                    })
                    .size(14)
                    .into();

                let file_row = row![
                    // Checkbox for selection
                    checkbox_element,
                    // Clickable filename (with tooltip if truncated)
                    filename_element,
                    // Type label
                    text(type_label).size(tiny).width(Length::Fixed(28.0)),
                    // Action button
                    action_button,
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .padding([2, 4]);

                items.push(file_row.into());
            }

            scrollable(
                Column::with_children(items)
                    .spacing(0)
                    .padding(iced::Padding::ZERO.right(12)), // Right padding for scrollbar clearance
            )
            .height(Length::Fill)
            .into()
        };

        column![
            nav_buttons,
            quick_nav,
            path_display,
            self.view_status_bar(small),
            file_list,
        ]
        .spacing(5)
        .padding(5)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::Alignment::Center)
        .into()
    }

    /// Build the status bar element — shows progress bar during transfers,
    /// "Loading..." during popup loads, or the regular status message.
    fn view_status_bar(&self, small: u32) -> Element<'_, RemoteBrowserMessage> {
        if let Some(prog) = self.get_progress() {
            // Truncate current filename for display
            let file_display = if prog.current_file.len() > 30 {
                format!("...{}", &prog.current_file[prog.current_file.len() - 27..])
            } else {
                prog.current_file.clone()
            };

            if prog.total > 0 {
                // Determinate progress: known total (e.g. batch of individual files)
                let pct = prog.current as f32 / prog.total as f32;
                column![
                    row![
                        text(format!(
                            "{} ({}/{})",
                            prog.operation, prog.current, prog.total
                        ))
                        .size(small),
                        Space::new().width(Length::Fill),
                        text(file_display).size(small),
                    ]
                    .spacing(5),
                    container(progress_bar(0.0..=1.0, pct)).height(Length::Fixed(6.0)),
                ]
                .spacing(2)
                .into()
            } else {
                // Indeterminate progress: unknown total (e.g. recursive directory download)
                // Show file count and current filename, bar pulses at 100% to indicate activity
                column![
                    row![
                        text(format!("{} ({} files)", prog.operation, prog.current)).size(small),
                        Space::new().width(Length::Fill),
                        text(file_display).size(small),
                    ]
                    .spacing(5),
                    container(progress_bar(0.0..=1.0, 1.0)).height(Length::Fixed(6.0)),
                ]
                .spacing(2)
                .into()
            }
        } else if self.disk_info_loading || self.content_preview_loading {
            text("Loading...").size(small).into()
        } else {
            text(self.status_message.as_deref().unwrap_or(""))
                .size(small)
                .into()
        }
    }

    fn view_create_disk_dialog(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        let header = row![
            text("💾 Create New Disk Image").size(normal),
            Space::new().width(Length::Fill),
            button(text("✖ Cancel").size(small))
                .on_press(RemoteBrowserMessage::CloseCreateDisk)
                .padding([4, 10]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(5);

        // Disk type radio buttons
        let type_row = row![
            text("Format:").size(small).width(Length::Fixed(70.0)),
            button(
                text(if self.create_disk_type == DiskCreateType::D64 {
                    "● D64"
                } else {
                    "○ D64"
                })
                .size(small)
            )
            .on_press(RemoteBrowserMessage::CreateDiskTypeChanged(
                DiskCreateType::D64
            ))
            .padding([4, 10]),
            button(
                text(if self.create_disk_type == DiskCreateType::D71 {
                    "● D71"
                } else {
                    "○ D71"
                })
                .size(small)
            )
            .on_press(RemoteBrowserMessage::CreateDiskTypeChanged(
                DiskCreateType::D71
            ))
            .padding([4, 10]),
            button(
                text(if self.create_disk_type == DiskCreateType::D81 {
                    "● D81"
                } else {
                    "○ D81"
                })
                .size(small)
            )
            .on_press(RemoteBrowserMessage::CreateDiskTypeChanged(
                DiskCreateType::D81
            ))
            .padding([4, 10]),
            Space::new().width(10),
            text(format!("({})", self.create_disk_type)).size(tiny),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Disk name input
        let name_row = row![
            text("Name:").size(small).width(Length::Fixed(70.0)),
            text_input("DISK NAME (max 16 chars)", &self.create_disk_name)
                .on_input(RemoteBrowserMessage::CreateDiskNameChanged)
                .size(normal)
                .padding(4)
                .width(Length::Fixed(200.0)),
            Space::new().width(10),
            text(format!("{}/16 chars", self.create_disk_name.len())).size(tiny),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Disk ID input
        let id_row = row![
            text("ID:").size(small).width(Length::Fixed(70.0)),
            text_input("XX XX", &self.create_disk_id)
                .on_input(RemoteBrowserMessage::CreateDiskIdChanged)
                .size(normal)
                .padding(4)
                .width(Length::Fixed(80.0)),
            Space::new().width(10),
            text("2-char ID + DOS type (e.g. 01 2A)").size(tiny),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Destination info
        let dest_row = row![
            text("Dest:").size(small).width(Length::Fixed(70.0)),
            text(format!("{}/", self.current_path)).size(small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Filename preview
        let safe_name = self.create_disk_name.replace(' ', "_");
        let ext = match self.create_disk_type {
            DiskCreateType::D64 => "d64",
            DiskCreateType::D71 => "d71",
            DiskCreateType::D81 => "d81",
        };
        let filename_preview = format!("{}.{}", safe_name, ext);

        let preview_row = row![
            text("File:").size(small).width(Length::Fixed(70.0)),
            text(filename_preview).size(small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Confirm button
        let can_create =
            !self.create_disk_busy && !self.create_disk_name.is_empty() && self.is_connected;

        let confirm_btn = if can_create {
            button(
                text(if self.create_disk_busy {
                    "Creating…"
                } else {
                    "✔ Create & Upload"
                })
                .size(normal),
            )
            .on_press(RemoteBrowserMessage::CreateDiskConfirm)
            .padding([8, 20])
        } else {
            button(
                text(if self.create_disk_busy {
                    "Creating…"
                } else {
                    "✔ Create & Upload"
                })
                .size(normal),
            )
            .padding([8, 20])
        };

        container(
            column![
                header,
                rule::horizontal(1),
                column![type_row, name_row, id_row, dest_row, preview_row]
                    .spacing(10)
                    .padding(10),
                rule::horizontal(1),
                row![Space::new().width(Length::Fill), confirm_btn].padding([5, 0]),
            ]
            .spacing(8)
            .padding(10),
        )
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
    }

    fn view_disk_info_popup(
        &self,
        disk_info: &DiskInfo,
        font_size: u32,
    ) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        // Header with disk name and close button
        let header = row![
            text(format!("{} - ", disk_info.kind)).size(small),
            text(format!("\"{}\"", disk_info.name)).size(normal),
            Space::new().width(Length::Fill),
            text(format!("{} {}", disk_info.disk_id, disk_info.dos_type)).size(small),
            Space::new().width(10),
            tooltip(
                button(text("Close").size(small))
                    .on_press(RemoteBrowserMessage::CloseDiskInfo)
                    .padding([4, 10]),
                "Close directory listing",
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Show rendered C64-style PETSCII image if available,
        // otherwise fall back to plain text listing
        let listing: Element<'_, RemoteBrowserMessage> =
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
                let mut items: Vec<Element<'_, RemoteBrowserMessage>> = Vec::new();
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
                                .size(tiny)
                                .width(Length::Fixed(35.0)),
                            text(format!("\"{}\"", entry.name))
                                .size(tiny)
                                .width(Length::Fill),
                            text(format!(
                                "{}{}{}",
                                closed_indicator, entry.file_type, lock_indicator
                            ))
                            .size(tiny)
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
            text(format!("{} BLOCKS FREE", disk_info.blocks_free)).size(small),
            Space::new().width(Length::Fill),
            text(format!("{} files", disk_info.entries.len())).size(tiny),
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
        .style(container::bordered_box)
        .into()
    }

    fn view_content_preview_popup<'a>(
        &'a self,
        content: &'a ContentPreview,
        font_size: u32,
    ) -> Element<'a, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

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
                    text("TEXT - ").size(small),
                    text(display_name.clone()).size(normal),
                    Space::new().width(Length::Fill),
                    text(format!("{} lines", line_count)).size(small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10]),
                        "Close text preview",
                        tooltip::Position::Left,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                // Text content with line numbers
                let mut text_lines: Vec<Element<'_, RemoteBrowserMessage>> = Vec::new();
                for (i, line) in content.lines().enumerate() {
                    let line_row = row![
                        text(format!("{:>4}", i + 1))
                            .size(tiny)
                            .width(Length::Fixed(35.0))
                            .color(iced::Color::from_rgb(0.5, 0.5, 0.5)),
                        text(line).size(tiny),
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
                .style(container::bordered_box)
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
                    text("IMAGE - ").size(small),
                    text(display_name.clone()).size(normal),
                    Space::new().width(Length::Fill),
                    text(format!("{}x{}", width, height)).size(small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10]),
                        "Close image preview",
                        tooltip::Position::Left,
                    )
                    .style(container::bordered_box),
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
                .style(container::bordered_box)
                .into()
            }
        }
    }

    pub fn get_selected_file(&self) -> Option<&str> {
        self.selected_file.as_deref()
    }

    pub fn get_current_path(&self) -> &str {
        &self.current_path
    }

    pub fn get_checked_files(&self) -> Vec<String> {
        self.checked_files.iter().cloned().collect()
    }

    #[allow(dead_code)]
    pub fn clear_checked(&mut self) {
        self.checked_files.clear();
    }

    /// Subscription that ticks every 250ms while a transfer is in progress.
    /// Wire this into your main app's subscription alongside other subscriptions.
    pub fn subscription(&self) -> Subscription<RemoteBrowserMessage> {
        let has_progress = self
            .transfer_progress
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false);

        if has_progress {
            iced::time::every(Duration::from_millis(250))
                .map(|_| RemoteBrowserMessage::ProgressTick)
        } else {
            Subscription::none()
        }
    }

    /// Returns true if a file transfer is currently in progress
    pub fn is_transferring(&self) -> bool {
        self.transfer_progress
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Returns current transfer progress if a transfer is in progress
    fn get_progress(&self) -> Option<TransferProgress> {
        self.transfer_progress.lock().ok().and_then(|g| g.clone())
    }
}

// Get icon for file type
fn get_file_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".prg") {
        "PRG"
    } else if lower.ends_with(".d64")
        || lower.ends_with(".g64")
        || lower.ends_with(".d71")
        || lower.ends_with(".g71")
        || lower.ends_with(".d81")
    {
        "DSK"
    } else if lower.ends_with(".crt") {
        "CRT"
    } else if lower.ends_with(".sid") {
        "SID"
    } else if lower.ends_with(".mod") || lower.ends_with(".xm") || lower.ends_with(".s3m") {
        "MOD"
    } else if lower.ends_with(".tap") || lower.ends_with(".t64") {
        "TAP"
    } else if lower.ends_with(".reu") {
        "REU"
    } else if lower.ends_with(".pdf") {
        "PDF"
    } else if lower.ends_with(".txt")
        || lower.ends_with(".nfo")
        || lower.ends_with(".diz")
        || lower.ends_with(".atxt")
    {
        "TXT"
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".bmp")
    {
        "IMG"
    } else {
        ""
    }
}

/// Check if a remote file is a previewable text file (by name)
fn is_remote_text_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".txt")
        || lower.ends_with(".atxt")
        || lower.ends_with(".nfo")
        || lower.ends_with(".diz")
        || lower.starts_with("readme")
        || lower == "file_id.diz"
}

/// Check if a remote file is a previewable image file (by name)
fn is_remote_image_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".bmp")
}

// Fetch files via FTP
async fn fetch_files_ftp(
    host: String,
    path: String,
    password: Option<String>,
) -> Result<Vec<RemoteFileEntry>, String> {
    log::info!("FTP: Listing {} on {}", path, host);

    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::time::Duration;
            use suppaftp::FtpStream;

            // Connect to FTP server (port 21)
            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            // Set timeout
            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(10)))
                .ok();
            ftp.get_ref()
                .set_write_timeout(Some(Duration::from_secs(10)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            // Change to directory
            let ftp_path = if path.is_empty() || path == "/" {
                "/"
            } else {
                &path
            };
            ftp.cwd(ftp_path)
                .map_err(|e| format!("Cannot access {}: {}", ftp_path, e))?;

            // List directory
            let list = ftp
                .list(None)
                .map_err(|e| format!("FTP list failed: {}", e))?;

            let mut entries = Vec::new();

            for line in list {
                if let Some(entry) = parse_ftp_line(&line, &path) {
                    if entry.name != "." && entry.name != ".." {
                        entries.push(entry);
                    }
                }
            }

            // Sort: directories first, then by name
            entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            // Logout
            let _ = ftp.quit();

            Ok(entries)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP list timed out - device may be offline".to_string()),
    }
}

// Parse FTP LIST line (Unix or DOS format)
fn parse_ftp_line(line: &str, parent_path: &str) -> Option<RemoteFileEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // Try Unix format: drwxr-xr-x 2 owner group 4096 Jan 1 12:00 filename
    // Or: -rw-r--r-- 1 owner group 12345 Jan 1 12:00 filename
    if line.len() > 10 && (line.starts_with('d') || line.starts_with('-') || line.starts_with('l'))
    {
        let is_dir = line.starts_with('d');

        // Split by whitespace, filename is everything after the 8th field
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 9 {
            let size: u64 = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let name = parts[8..].join(" ");

            if name.is_empty() || name == "." || name == ".." {
                return None;
            }

            let entry_path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path.trim_end_matches('/'), name)
            };

            return Some(RemoteFileEntry {
                name,
                is_dir,
                size,
                path: entry_path,
            });
        }
    }

    // Try DOS/Windows format: 01-01-24 12:00PM <DIR> dirname
    // Or: 01-01-24 12:00PM 12345 filename
    if line.contains("<DIR>") {
        let parts: Vec<&str> = line.split("<DIR>").collect();
        if parts.len() == 2 {
            let name = parts[1].trim().to_string();
            if name.is_empty() || name == "." || name == ".." {
                return None;
            }
            let entry_path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path.trim_end_matches('/'), name)
            };
            return Some(RemoteFileEntry {
                name,
                is_dir: true,
                size: 0,
                path: entry_path,
            });
        }
    }

    // Simple format: just filename or "filename size"
    let parts: Vec<&str> = line.split_whitespace().collect();
    if !parts.is_empty() {
        let name = parts[0].to_string();
        let is_dir = name.ends_with('/');
        let name = name.trim_end_matches('/').to_string();

        if name.is_empty() || name == "." || name == ".." {
            return None;
        }

        let size: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

        let entry_path = if parent_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent_path.trim_end_matches('/'), name)
        };

        return Some(RemoteFileEntry {
            name,
            is_dir,
            size,
            path: entry_path,
        });
    }

    None
}

// Download file via FTP with longer timeout for previews
async fn download_file_ftp_preview(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<(String, Vec<u8>), String> {
    log::info!("FTP: Downloading preview {}", remote_path);

    // Use longer timeout for preview downloads
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_PREVIEW_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(120)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            // Set binary transfer mode
            ftp.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            // Get filename from path
            let filename = remote_path.rsplit('/').next().unwrap_or("file").to_string();

            // Retrieve file
            let mut reader = ftp
                .retr_as_stream(&remote_path)
                .map_err(|e| format!("FTP download failed: {}", e))?;

            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .map_err(|e| format!("Read error: {}", e))?;

            // Finalize transfer
            ftp.finalize_retr_stream(reader)
                .map_err(|e| format!("Transfer finalize error: {}", e))?;

            let _ = ftp.quit();

            Ok((filename, data))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Download timed out - file may be too large".to_string()),
    }
}

// Download file via FTP
/// Single file download with progress reporting
async fn download_file_ftp_with_progress(
    host: String,
    remote_path: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<(String, Vec<u8>), String> {
    let result = download_file_ftp(host, remote_path, password).await;
    // Mark progress as done
    if let Ok(mut g) = progress.lock() {
        if let Some(ref mut p) = *g {
            p.current = 1;
            p.done = true;
        }
    }
    result
}

// Download file via FTP
async fn download_file_ftp(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<(String, Vec<u8>), String> {
    log::info!("FTP: Downloading {}", remote_path);

    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(60)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            // Set binary transfer mode
            ftp.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            // Get filename from path
            let filename = remote_path.rsplit('/').next().unwrap_or("file").to_string();

            // Retrieve file
            let mut reader = ftp
                .retr_as_stream(&remote_path)
                .map_err(|e| format!("FTP download failed: {}", e))?;

            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .map_err(|e| format!("Read error: {}", e))?;

            // Finalize transfer
            ftp.finalize_retr_stream(reader)
                .map_err(|e| format!("Transfer finalize error: {}", e))?;

            let _ = ftp.quit();

            Ok((filename, data))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP download timed out - device may be offline".to_string()),
    }
}

// Upload file via FTP
async fn upload_file_ftp(
    host: String,
    local_path: PathBuf,
    remote_dest: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    log::info!("FTP: Uploading {} to {}", local_path.display(), remote_dest);

    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Cursor;
            use std::time::Duration;
            use suppaftp::FtpStream;

            // Read local file
            let data =
                std::fs::read(&local_path).map_err(|e| format!("Cannot read file: {}", e))?;

            let filename = local_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_write_timeout(Some(Duration::from_secs(120)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            // Set binary transfer mode
            ftp.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            // Change to destination directory
            let dest_dir = if remote_dest.ends_with('/') {
                remote_dest.as_str()
            } else {
                remote_dest.rsplit_once('/').map(|(d, _)| d).unwrap_or("/")
            };

            ftp.cwd(dest_dir)
                .map_err(|e| format!("Cannot access {}: {}", dest_dir, e))?;

            // Upload file
            let mut cursor = Cursor::new(data);
            ftp.put_file(&filename, &mut cursor)
                .map_err(|e| format!("FTP upload failed: {}", e))?;

            let _ = ftp.quit();

            // Mark progress as done
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current = 1;
                    p.done = true;
                }
            }

            Ok(filename)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP upload timed out - device may be offline".to_string()),
    }
}

// Upload directory recursively via FTP
async fn upload_directory_ftp(
    host: String,
    local_path: PathBuf,
    remote_dest: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    log::info!(
        "FTP: Uploading directory {} to {}",
        local_path.display(),
        remote_dest
    );

    // Use longer timeout for directory uploads which may take time
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_UPLOAD_DIR_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Cursor;
            use std::time::Duration;
            use suppaftp::FtpStream;

            // Count total files for progress (quick pre-scan)
            let total_files = WalkDir::new(&local_path)
                .min_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .count();

            // Initialize progress
            if let Ok(mut g) = progress.lock() {
                *g = Some(TransferProgress {
                    current: 0,
                    total: total_files,
                    current_file: String::new(),
                    operation: "Uploading".to_string(),
                    done: false,
                });
            }

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_write_timeout(Some(Duration::from_secs(120)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            // Set binary transfer mode
            ftp.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            // Get the directory name to create on remote
            let dir_name = local_path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| "Invalid directory name".to_string())?;

            // Build base remote path
            let base_remote = if remote_dest.ends_with('/') {
                format!("{}{}", remote_dest, dir_name)
            } else {
                format!("{}/{}", remote_dest, dir_name)
            };

            let mut dirs_created = 0;
            let mut files_uploaded = 0;
            let mut errors: Vec<String> = Vec::new();

            // Walk the directory tree
            for entry in WalkDir::new(&local_path).min_depth(0) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        errors.push(format!("Walk error: {}", e));
                        continue;
                    }
                };

                // Get relative path from the source directory
                let relative = match entry.path().strip_prefix(&local_path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Build remote path
                let remote_path = if relative.as_os_str().is_empty() {
                    base_remote.clone()
                } else {
                    // Convert path separators to forward slashes for FTP
                    let relative_str = relative.to_string_lossy().replace('\\', "/");
                    format!("{}/{}", base_remote, relative_str)
                };

                if entry.file_type().is_dir() {
                    // Create directory on remote (ignore errors if it exists)
                    log::debug!("FTP: Creating directory {}", remote_path);
                    match ftp.mkdir(&remote_path) {
                        Ok(_) => {
                            dirs_created += 1;
                            log::debug!("FTP: Created directory {}", remote_path);
                        }
                        Err(e) => {
                            // Directory might already exist, log but continue
                            log::debug!("FTP: mkdir {} (may exist): {}", remote_path, e);
                        }
                    }
                } else if entry.file_type().is_file() {
                    // Update progress with current filename
                    let filename_display = entry
                        .path()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.current_file = filename_display;
                        }
                    }

                    // Upload file
                    log::debug!("FTP: Uploading file to {}", remote_path);

                    // Read file data
                    let data = match std::fs::read(entry.path()) {
                        Ok(d) => d,
                        Err(e) => {
                            errors.push(format!("Read {}: {}", entry.path().display(), e));
                            continue;
                        }
                    };

                    // Get parent directory and filename
                    let (parent_dir, filename) = if let Some(pos) = remote_path.rfind('/') {
                        (&remote_path[..pos], &remote_path[pos + 1..])
                    } else {
                        ("/", remote_path.as_str())
                    };

                    // Change to parent directory
                    if let Err(e) = ftp.cwd(parent_dir) {
                        errors.push(format!("CWD {}: {}", parent_dir, e));
                        continue;
                    }

                    // Upload the file
                    let mut cursor = Cursor::new(data);
                    match ftp.put_file(filename, &mut cursor) {
                        Ok(_) => {
                            files_uploaded += 1;
                            log::debug!("FTP: Uploaded {}", remote_path);
                        }
                        Err(e) => {
                            errors.push(format!("Upload {}: {}", filename, e));
                        }
                    }

                    // Update progress count
                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.current = files_uploaded;
                        }
                    }
                }
            }

            let _ = ftp.quit();

            // Mark progress as done
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.done = true;
                }
            }

            // Build result message
            let mut msg = format!(
                "Uploaded: {} files, {} directories",
                files_uploaded, dirs_created
            );
            if !errors.is_empty() {
                msg.push_str(&format!(" ({} errors)", errors.len()));
                for err in errors.iter().take(3) {
                    log::warn!("Upload error: {}", err);
                }
            }

            Ok(msg)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP directory upload timed out - device may be offline".to_string()),
    }
}

/// Download multiple files and directories via FTP to a local directory
async fn download_batch_ftp(
    host: String,
    file_paths: Vec<String>,
    dir_paths: Vec<String>,
    local_dest: PathBuf,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    log::info!(
        "FTP: Batch downloading {} files and {} directories to {}",
        file_paths.len(),
        dir_paths.len(),
        local_dest.display()
    );

    // Initialize progress (files + directories as total items)
    let total = file_paths.len() + dir_paths.len();
    if let Ok(mut g) = progress.lock() {
        *g = Some(TransferProgress {
            current: 0,
            total,
            current_file: String::new(),
            operation: "Downloading".to_string(),
            done: false,
        });
    }

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_UPLOAD_DIR_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(60)))
                .ok();

            // Login with password or anonymous
            if let Some(ref pwd) = password {
                if !pwd.is_empty() {
                    ftp.login("admin", pwd)
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                } else {
                    ftp.login("anonymous", "anonymous")
                        .map_err(|e| format!("FTP login failed: {}", e))?;
                }
            } else {
                ftp.login("anonymous", "anonymous")
                    .map_err(|e| format!("FTP login failed: {}", e))?;
            }

            ftp.transfer_type(suppaftp::types::FileType::Binary)
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let mut files_downloaded = 0;
            let mut dirs_downloaded = 0;
            let mut items_completed = 0;
            let mut errors: Vec<String> = Vec::new();

            // Download individual files
            for remote_path in &file_paths {
                let filename = remote_path.rsplit('/').next().unwrap_or("file");
                let local_path = local_dest.join(filename);

                // Update progress with current filename
                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current_file = filename.to_string();
                    }
                }

                log::debug!(
                    "FTP: Downloading file {} to {}",
                    remote_path,
                    local_path.display()
                );

                match ftp.retr_as_stream(remote_path) {
                    Ok(mut reader) => {
                        let mut data = Vec::new();
                        if let Err(e) = reader.read_to_end(&mut data) {
                            errors.push(format!("Read {}: {}", filename, e));
                            continue;
                        }
                        if let Err(e) = ftp.finalize_retr_stream(reader) {
                            errors.push(format!("Finalize {}: {}", filename, e));
                            continue;
                        }
                        if let Err(e) = std::fs::write(&local_path, &data) {
                            errors.push(format!("Write {}: {}", filename, e));
                            continue;
                        }
                        files_downloaded += 1;
                    }
                    Err(e) => {
                        errors.push(format!("Download {}: {}", filename, e));
                    }
                }

                // Update progress count
                items_completed += 1;
                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current = items_completed;
                    }
                }
            }

            // Download directories recursively
            for remote_dir in &dir_paths {
                let dir_name = remote_dir.rsplit('/').next().unwrap_or("dir");
                let local_dir = local_dest.join(dir_name);

                // Switch progress to per-file mode for this directory
                // total=0 signals indeterminate (we don't know how many files are inside)
                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current = 0;
                        p.total = 0;
                        p.current_file = format!("{}/", dir_name);
                        p.operation = "Downloading".to_string();
                    }
                }

                log::debug!(
                    "FTP: Downloading directory {} to {}",
                    remote_dir,
                    local_dir.display()
                );

                match download_directory_recursive(&mut ftp, remote_dir, &local_dir, &progress) {
                    Ok((files, dirs)) => {
                        files_downloaded += files;
                        dirs_downloaded += dirs;
                    }
                    Err(e) => {
                        errors.push(format!("Dir {}: {}", dir_name, e));
                    }
                }
            }

            let _ = ftp.quit();

            // Mark progress as done
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.done = true;
                }
            }

            let mut msg = format!(
                "Downloaded: {} files, {} directories",
                files_downloaded, dirs_downloaded
            );
            if !errors.is_empty() {
                msg.push_str(&format!(" ({} errors)", errors.len()));
                for err in errors.iter().take(3) {
                    log::warn!("Download error: {}", err);
                }
            }
            Ok(msg)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP batch download timed out - device may be offline".to_string()),
    }
}

/// Recursively download a remote directory via FTP
fn download_directory_recursive(
    ftp: &mut suppaftp::FtpStream,
    remote_path: &str,
    local_path: &std::path::Path,
    progress: &Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<(usize, usize), String> {
    use std::io::Read;

    std::fs::create_dir_all(local_path)
        .map_err(|e| format!("Create dir {}: {}", local_path.display(), e))?;

    let mut files_count = 0;
    let mut dirs_count = 1; // Count this directory

    // List remote directory contents
    let entries = ftp
        .list(Some(remote_path))
        .map_err(|e| format!("List {}: {}", remote_path, e))?;

    for entry_line in &entries {
        // Parse FTP LIST output (Unix-style: "drwxr-xr-x ... name")
        let parts: Vec<&str> = entry_line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name = parts[8..].join(" ");
        if name == "." || name == ".." {
            continue;
        }

        let is_dir = entry_line.starts_with('d');
        let child_remote = format!("{}/{}", remote_path.trim_end_matches('/'), name);
        let child_local = local_path.join(&name);

        if is_dir {
            match download_directory_recursive(ftp, &child_remote, &child_local, progress) {
                Ok((f, d)) => {
                    files_count += f;
                    dirs_count += d;
                }
                Err(e) => {
                    log::warn!("Skip dir {}: {}", child_remote, e);
                }
            }
        } else {
            // Update progress with current filename
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current_file = name.clone();
                }
            }

            match ftp.retr_as_stream(&child_remote) {
                Ok(mut reader) => {
                    let mut data = Vec::new();
                    if reader.read_to_end(&mut data).is_ok() {
                        let _ = ftp.finalize_retr_stream(reader);
                        if std::fs::write(&child_local, &data).is_ok() {
                            files_count += 1;

                            // Update progress count (increment current for each file)
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current += 1;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Skip file {}: {}", child_remote, e);
                }
            }
        }
    }

    Ok((files_count, dirs_count))
}

/// Download a remote disk image via FTP and parse its contents
async fn load_remote_disk_info(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<DiskInfo, String> {
    log::info!("FTP: Loading disk info for {}", remote_path);

    // Download the file first
    let (_, data) = download_file_ftp(host, remote_path, password).await?;

    // Parse the disk image from bytes
    tokio::task::spawn_blocking(move || disk_image::read_disk_info_from_bytes(&data))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

fn is_remote_pdf_file(name: &str) -> bool {
    name.to_lowercase().ends_with(".pdf")
}

/// Download a remote file via FTP and create a content preview
async fn load_remote_content_preview(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<ContentPreview, String> {
    log::info!("FTP: Loading content preview for {}", remote_path);

    // Get filename from path
    let filename = remote_path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    // Download the file first
    let (_, data) = download_file_ftp_preview(host, remote_path.clone(), password).await?;

    // Determine if text, image, or PDF based on filename
    if is_remote_text_file(&filename) {
        // Parse as text
        tokio::task::spawn_blocking(move || {
            let lower = filename.to_lowercase();

            // For PETSCII text files (.atxt), convert from PETSCII
            let content = if lower.ends_with(".atxt") {
                crate::petscii::convert_text_file(&data)
            } else {
                // Regular text file - try UTF-8, fall back to lossy conversion
                match String::from_utf8(data.clone()) {
                    Ok(s) => s,
                    Err(_) => String::from_utf8_lossy(&data).to_string(),
                }
            };

            let line_count = content.lines().count();

            Ok(ContentPreview::Text {
                filename,
                content,
                line_count,
            })
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    } else if is_remote_image_file(&filename) {
        // Parse as image
        tokio::task::spawn_blocking(move || {
            // Decode image to get dimensions
            let img = image::load_from_memory(&data)
                .map_err(|e| format!("Failed to decode image: {}", e))?;

            let width = img.width();
            let height = img.height();

            Ok(ContentPreview::Image {
                filename,
                data,
                width,
                height,
            })
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    } else if is_remote_pdf_file(&filename) {
        // Parse as PDF
        crate::pdf_preview::load_pdf_preview_from_bytes_async(data, filename).await
    } else {
        Err("Unsupported file type for preview".to_string())
    }
}

// ─── Disk image creation ──────────────────────────────────────────────────────

/// Create a blank disk image and upload it to the device via FTP.
fn create_and_upload_disk(
    host: String,
    name: String,
    disk_id: String,
    disk_type: DiskCreateType,
    remote_dest: String,
    password: Option<String>,
) -> Result<String, String> {
    use std::io::Cursor;
    use std::time::Duration;
    use suppaftp::FtpStream;

    let safe_name = name.replace(' ', "_");
    let (ext, data) = match disk_type {
        DiskCreateType::D64 => ("d64", disk_image::build_blank_d64(&name, &disk_id)),
        DiskCreateType::D71 => ("d71", disk_image::build_blank_d71(&name, &disk_id)),
        DiskCreateType::D81 => ("d81", disk_image::build_blank_d81(&name, &disk_id)),
    };
    let filename = format!("{}.{}", safe_name, ext);
    let remote_path = format!("{}/{}", remote_dest.trim_end_matches('/'), filename);

    log::info!(
        "Creating {} ({} bytes) → {}",
        filename,
        data.len(),
        remote_path
    );

    let addr = format!("{}:21", host);
    let mut ftp = FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
    ftp.get_ref()
        .set_write_timeout(Some(Duration::from_secs(60)))
        .ok();

    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            ftp.login("admin", pwd)
                .map_err(|e| format!("FTP login failed: {}", e))?;
        } else {
            ftp.login("anonymous", "anonymous")
                .map_err(|e| format!("FTP login failed: {}", e))?;
        }
    } else {
        ftp.login("anonymous", "anonymous")
            .map_err(|e| format!("FTP login failed: {}", e))?;
    }

    ftp.transfer_type(suppaftp::types::FileType::Binary)
        .map_err(|e| format!("Failed to set binary mode: {}", e))?;

    let dest_dir = remote_dest.trim_end_matches('/');
    ftp.cwd(dest_dir)
        .map_err(|e| format!("Cannot cd to {}: {}", dest_dir, e))?;

    let mut cursor = Cursor::new(data);
    ftp.put_file(&filename, &mut cursor)
        .map_err(|e| format!("FTP upload failed: {}", e))?;

    ftp.quit().ok();
    Ok(remote_path)
}
