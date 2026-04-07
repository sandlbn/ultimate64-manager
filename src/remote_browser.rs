use iced::{
    widget::{
        button, checkbox, column, container, pick_list, progress_bar, row, rule, scrollable, text,
        text_input, tooltip, Column, Space,
    },
    Element, Length, Subscription, Task,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::api;
use crate::dir_preview::ContentPreview;
use crate::disk_image::{DiskInfo, FileType};
use crate::file_browser::DriveOption;
use crate::ftp_ops::*;

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

    // ── Delete ────────────────────────────────────────────────────────────────
    /// Request deletion of a single file/dir (shows confirm dialog)
    DeleteFile(String),
    /// Request deletion of all currently checked files/dirs (shows confirm dialog)
    DeleteChecked,
    /// User confirmed the pending deletion — execute it
    DeleteConfirm,
    /// User cancelled the pending deletion
    DeleteCancel,
    /// Async deletion finished
    DeleteComplete(Result<String, String>),

    // ── Rename ───────────────────────────────────────────────────────────────
    /// Open the rename dialog for a given remote path
    RenameFile(String),
    /// User is typing the new name
    RenameInputChanged(String),
    /// User confirmed the rename — execute it
    RenameConfirm,
    /// User cancelled the rename
    RenameCancel,
    /// Async rename finished
    RenameComplete(Result<String, String>),

    // ── Create Directory ──────────────────────────────────────────────────
    /// Show the create directory dialog
    ShowCreateDir,
    /// User is typing the directory name
    CreateDirNameChanged(String),
    /// User confirmed — create the directory
    CreateDirConfirm,
    /// User cancelled
    CreateDirCancel,
    /// Async mkdir finished
    CreateDirComplete(Result<String, String>),

    // ── Sort ─────────────────────────────────────────────────────────────
    /// Change sort column (or toggle order if same column)
    SortBy(crate::file_types::SortColumn),

    /// Drive picker changed
    DriveSelected(DriveOption),
}

/// State for the delete confirmation dialog
#[derive(Debug, Clone)]
struct DeletePending {
    /// All paths that will be deleted (files and/or directories)
    paths: Vec<String>,
    /// Human-readable summary shown in the dialog
    summary: String,
}

/// State for the rename dialog
#[derive(Debug, Clone)]
struct RenamePending {
    /// Full remote path of the item being renamed
    original_path: String,
    /// Current value of the name text input
    new_name: String,
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

    // Delete confirm dialog
    delete_pending: Option<DeletePending>,
    // Rename dialog
    rename_pending: Option<RenamePending>,
    // Create directory dialog
    show_create_dir: bool,
    create_dir_name: String,
    // Sort state
    sort_column: crate::file_types::SortColumn,
    sort_order: crate::file_types::SortOrder,
    // Drive selector
    pub selected_drive: DriveOption,
    // Cached root directory names for quick nav
    root_dirs: Vec<String>,
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
            delete_pending: None,
            rename_pending: None,
            show_create_dir: false,
            create_dir_name: String::new(),
            sort_column: crate::file_types::SortColumn::Name,
            sort_order: crate::file_types::SortOrder::Ascending,
            selected_drive: DriveOption::A,
            root_dirs: Vec::new(),
        }
    }
}

impl RemoteBrowser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_host(&mut self, host: Option<String>, password: Option<String>) {
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
                        // Cache root directories for the quick nav bar
                        if self.current_path == "/" {
                            self.root_dirs = files
                                .iter()
                                .filter(|f| f.is_dir)
                                .map(|f| f.name.clone())
                                .collect();
                        }
                        self.files = files;
                        self.sort_files();
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
                    Ok(msg) => self.status_message = Some(msg),
                    Err(e) => self.status_message = Some(format!("Failed: {}", e)),
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
                    Ok(msg) => self.status_message = Some(msg),
                    Err(e) => self.status_message = Some(format!("Mount failed: {}", e)),
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
                        RemoteBrowserMessage::MountComplete,
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
                    Ok(preview) => self.content_preview = Some(preview),
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
                if let Ok(guard) = self.transfer_progress.lock() {
                    if let Some(ref progress) = *guard {
                        if progress.done {
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

            // ── Delete ────────────────────────────────────────────────────────
            RemoteBrowserMessage::DeleteFile(path) => {
                // Single file/dir delete — show confirmation dialog
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let is_dir = self
                    .files
                    .iter()
                    .find(|f| f.path == path)
                    .map(|f| f.is_dir)
                    .unwrap_or(false);
                let summary = if is_dir {
                    format!("Delete directory \"{}\" and ALL its contents?", name)
                } else {
                    format!("Delete file \"{}\"?", name)
                };
                self.delete_pending = Some(DeletePending {
                    paths: vec![path],
                    summary,
                });
                Task::none()
            }

            RemoteBrowserMessage::DeleteChecked => {
                if self.checked_files.is_empty() {
                    self.status_message = Some("No files selected".to_string());
                    return Task::none();
                }
                let paths: Vec<String> = self.checked_files.iter().cloned().collect();
                let file_count = paths
                    .iter()
                    .filter(|p| {
                        self.files
                            .iter()
                            .find(|f| &f.path == *p)
                            .map(|f| !f.is_dir)
                            .unwrap_or(true)
                    })
                    .count();
                let dir_count = paths.len() - file_count;

                let summary = match (file_count, dir_count) {
                    (f, 0) => format!("Delete {} file(s)?", f),
                    (0, d) => format!(
                        "Delete {} director{}? (recursive)",
                        d,
                        if d == 1 { "y" } else { "ies" }
                    ),
                    (f, d) => format!(
                        "Delete {} file(s) and {} director{}? (recursive)",
                        f,
                        d,
                        if d == 1 { "y" } else { "ies" }
                    ),
                };
                self.delete_pending = Some(DeletePending { paths, summary });
                Task::none()
            }

            RemoteBrowserMessage::DeleteCancel => {
                self.delete_pending = None;
                Task::none()
            }

            RemoteBrowserMessage::DeleteConfirm => {
                let pending = match self.delete_pending.take() {
                    Some(p) => p,
                    None => return Task::none(),
                };
                if let Some(host) = &self.host_address {
                    let count = pending.paths.len();
                    self.status_message = Some(format!("Deleting {} item(s)...", count));
                    if let Ok(mut g) = self.transfer_progress.lock() {
                        *g = Some(TransferProgress {
                            current: 0,
                            total: count,
                            current_file: String::new(),
                            operation: "Deleting".to_string(),
                            done: false,
                        });
                    }
                    let host = host.clone();
                    let password = self.password.clone();
                    let progress = self.transfer_progress.clone();
                    // Collect is_dir flags so the async fn knows what each path is
                    let paths_with_type: Vec<(String, bool)> = pending
                        .paths
                        .iter()
                        .map(|p| {
                            let is_dir = self
                                .files
                                .iter()
                                .find(|f| &f.path == p)
                                .map(|f| f.is_dir)
                                .unwrap_or(false);
                            (p.clone(), is_dir)
                        })
                        .collect();
                    Task::perform(
                        delete_ftp(host, paths_with_type, password, progress),
                        RemoteBrowserMessage::DeleteComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::DeleteComplete(result) => {
                if let Ok(mut g) = self.transfer_progress.lock() {
                    *g = None;
                }
                self.checked_files.clear();
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Delete failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Rename ───────────────────────────────────────────────────────
            RemoteBrowserMessage::RenameFile(path) => {
                let current_name = path.rsplit('/').next().unwrap_or("").to_string();
                self.rename_pending = Some(RenamePending {
                    original_path: path,
                    new_name: current_name,
                });
                Task::none()
            }

            RemoteBrowserMessage::RenameInputChanged(value) => {
                if let Some(ref mut rp) = self.rename_pending {
                    rp.new_name = value;
                }
                Task::none()
            }

            RemoteBrowserMessage::RenameCancel => {
                self.rename_pending = None;
                Task::none()
            }

            RemoteBrowserMessage::RenameConfirm => {
                let pending = match self.rename_pending.take() {
                    Some(p) => p,
                    None => return Task::none(),
                };
                if pending.new_name.trim().is_empty() {
                    self.status_message = Some("Name cannot be empty".to_string());
                    return Task::none();
                }
                // Build new path: same parent dir, new name
                let parent = pending
                    .original_path
                    .rsplit_once('/')
                    .map(|(p, _)| p)
                    .unwrap_or("/");
                let new_path = if parent == "" || parent == "/" {
                    format!("/{}", pending.new_name.trim())
                } else {
                    format!("{}/{}", parent, pending.new_name.trim())
                };
                if new_path == pending.original_path {
                    self.rename_pending = None;
                    return Task::none();
                }
                if let Some(host) = &self.host_address {
                    self.status_message =
                        Some(format!("Renaming to {}...", pending.new_name.trim()));
                    let host = host.clone();
                    let password = self.password.clone();
                    let old = pending.original_path.clone();
                    Task::perform(
                        rename_ftp(host, old, new_path, password),
                        RemoteBrowserMessage::RenameComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Task::none()
                }
            }

            RemoteBrowserMessage::RenameComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Rename failed: {}", e));
                    }
                }
                Task::none()
            }

            RemoteBrowserMessage::SortBy(col) => {
                if self.sort_column == col {
                    self.sort_order = self.sort_order.toggle();
                } else {
                    self.sort_column = col;
                    self.sort_order = crate::file_types::SortOrder::Ascending;
                }
                self.sort_files();
                Task::none()
            }

            RemoteBrowserMessage::DriveSelected(drive) => {
                self.selected_drive = drive;
                Task::none()
            }

            // ── Create Directory ─────────────────────────────────────────────
            RemoteBrowserMessage::ShowCreateDir => {
                self.show_create_dir = true;
                self.create_dir_name = String::new();
                Task::none()
            }
            RemoteBrowserMessage::CreateDirNameChanged(name) => {
                self.create_dir_name = name;
                Task::none()
            }
            RemoteBrowserMessage::CreateDirCancel => {
                self.show_create_dir = false;
                Task::none()
            }
            RemoteBrowserMessage::CreateDirConfirm => {
                if self.create_dir_name.trim().is_empty() {
                    return Task::none();
                }
                let dir_name = self.create_dir_name.trim().to_string();
                let remote_path =
                    format!("{}/{}", self.current_path.trim_end_matches('/'), dir_name);
                let host = self.host_address.clone().unwrap_or_default();
                let password = self.password.clone();
                self.show_create_dir = false;

                Task::perform(
                    crate::ftp_ops::mkdir_ftp(host, remote_path, password),
                    RemoteBrowserMessage::CreateDirComplete,
                )
            }
            RemoteBrowserMessage::CreateDirComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        // Refresh files to show new directory
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error: {}", e));
                    }
                }
                Task::none()
            }
        }
    }

    fn sort_files(&mut self) {
        use crate::file_types::{SortColumn, SortOrder};
        let col = self.sort_column;
        let ord = self.sort_order;
        self.files.sort_by(|a, b| {
            // Directories always come first
            match (a.is_dir, b.is_dir) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
            let cmp = match col {
                SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortColumn::Size => a.size.cmp(&b.size),
                SortColumn::Type => {
                    let ext_a = a.name.rsplit('.').next().unwrap_or("").to_lowercase();
                    let ext_b = b.name.rsplit('.').next().unwrap_or("").to_lowercase();
                    ext_a.cmp(&ext_b)
                }
            };
            if ord == SortOrder::Descending {
                cmp.reverse()
            } else {
                cmp
            }
        });
    }

    // ── Builder helper methods for Total Commander-style layout ──────────

    fn build_nav_row(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let display_path = if self.current_path.len() > 35 {
            format!("...{}", &self.current_path[self.current_path.len() - 32..])
        } else {
            self.current_path.clone()
        };
        row![
            tooltip(
                button(text("⬆").size(font_size))
                    .on_press(RemoteBrowserMessage::NavigateUp)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                "Go to parent folder",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
            text(display_path).size(small).width(Length::Fill),
            tooltip(
                button(text("⟳").size(small))
                    .on_press(RemoteBrowserMessage::RefreshFiles)
                    .padding([2, 6])
                    .style(crate::styles::nav_button),
                "Refresh file listing",
                tooltip::Position::Bottom,
            )
            .style(crate::styles::subtle_tooltip),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center)
        .into()
    }

    fn build_quick_nav_row(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let mut nav = row![tooltip(
            button(text("/").size(small))
                .on_press(RemoteBrowserMessage::NavigateToPath("/".to_string()))
                .padding([2, 6])
                .style(crate::styles::nav_button),
            "Root directory",
            tooltip::Position::Bottom,
        )
        .style(crate::styles::subtle_tooltip),]
        .spacing(3)
        .align_y(iced::Alignment::Center);

        if self.root_dirs.is_empty() {
            // Fallback: show default drives before first root listing
            for name in &["Usb0", "SD"] {
                let path = format!("/{}", name);
                nav = nav.push(
                    tooltip(
                        button(text(*name).size(small))
                            .on_press(RemoteBrowserMessage::NavigateToPath(path))
                            .padding([2, 6])
                            .style(crate::styles::nav_button),
                        *name,
                        tooltip::Position::Bottom,
                    )
                    .style(crate::styles::subtle_tooltip),
                );
            }
        } else {
            // Show actual root directories from the device
            for dir_name in &self.root_dirs {
                let path = format!("/{}", dir_name);
                nav = nav.push(
                    tooltip(
                        button(text(dir_name.as_str()).size(small))
                            .on_press(RemoteBrowserMessage::NavigateToPath(path))
                            .padding([2, 6])
                            .style(crate::styles::nav_button),
                        text(format!("Navigate to /{}", dir_name)).size(small),
                        tooltip::Position::Bottom,
                    )
                    .style(crate::styles::subtle_tooltip),
                );
            }
        }

        nav.into()
    }

    fn build_status_bar(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let tiny = fs.tiny;
        let small = fs.small;
        let file_count = self.files.len();
        let checked_count = self.checked_files.len();

        // Show progress bar during transfers (delegate to existing view_status_bar)
        if self.get_progress().is_some() {
            return self.view_status_bar(small);
        }

        if self.disk_info_loading || self.content_preview_loading {
            return self.view_status_bar(small);
        }

        let mut items = row![].spacing(8).align_y(iced::Alignment::Center);

        items = items.push(text(format!("{} files", file_count)).size(tiny));

        if checked_count > 0 {
            items = items.push(text("|").size(tiny));
            items = items.push(text(format!("{} sel", checked_count)).size(tiny));
        }

        items = items.push(
            pick_list(
                DriveOption::get_all(),
                Some(self.selected_drive.clone()),
                RemoteBrowserMessage::DriveSelected,
            )
            .placeholder("Drive")
            .text_size(tiny)
            .width(Length::Fixed(95.0)),
        );

        items = items.push(Space::new().width(Length::Fill));

        if self.is_connected {
            items = items.push(text("Connected").size(tiny));
        }

        items.into()
    }

    fn build_column_headers(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;

        let name_indicator = if self.sort_column == crate::file_types::SortColumn::Name {
            self.sort_order.indicator()
        } else {
            ""
        };
        let size_indicator = if self.sort_column == crate::file_types::SortColumn::Size {
            self.sort_order.indicator()
        } else {
            ""
        };
        let type_indicator = if self.sort_column == crate::file_types::SortColumn::Type {
            self.sort_order.indicator()
        } else {
            ""
        };

        row![
            Space::new().width(24), // checkbox space
            button(text(format!("Name{}", name_indicator)).size(small))
                .on_press(RemoteBrowserMessage::SortBy(
                    crate::file_types::SortColumn::Name
                ))
                .padding([2, 4])
                .style(button::text),
            Space::new().width(Length::Fill),
            button(text(format!("Size{}", size_indicator)).size(small))
                .on_press(RemoteBrowserMessage::SortBy(
                    crate::file_types::SortColumn::Size
                ))
                .padding([2, 4])
                .width(Length::Fixed(65.0))
                .style(button::text),
            button(text(format!("Type{}", type_indicator)).size(small))
                .on_press(RemoteBrowserMessage::SortBy(
                    crate::file_types::SortColumn::Type
                ))
                .padding([2, 4])
                .width(Length::Fixed(35.0))
                .style(button::text),
            Space::new().width(Length::Shrink), // action buttons space
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub fn view(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;
        let tiny = fs.tiny;

        // ── Delete confirm dialog — shown over everything else ─────────────
        if let Some(ref dp) = self.delete_pending {
            let dialog = self.view_delete_confirm_dialog(dp, font_size);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // ── Rename dialog ─────────────────────────────────────────────────
        if let Some(ref rp) = self.rename_pending {
            let dialog = self.view_rename_dialog(rp, font_size);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        if self.show_create_disk {
            let dialog = self.view_create_disk_dialog(font_size);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // ── Create directory dialog ────────────────────────────────────────
        if self.show_create_dir {
            let small = fs.small;
            let dialog = container(
                column![
                    text("Create Directory").size(font_size),
                    row![
                        text("Name:").size(small),
                        text_input("directory name...", &self.create_dir_name)
                            .on_input(RemoteBrowserMessage::CreateDirNameChanged)
                            .on_submit(RemoteBrowserMessage::CreateDirConfirm)
                            .size(small as f32)
                            .padding(4)
                            .width(Length::Fixed(200.0)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                    row![
                        button(text("Create").size(small))
                            .on_press(RemoteBrowserMessage::CreateDirConfirm)
                            .padding([5, 15])
                            .style(button::secondary),
                        button(text("Cancel").size(small))
                            .on_press(RemoteBrowserMessage::CreateDirCancel)
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

            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                dialog,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        if let Some(disk_info) = &self.disk_info_popup {
            let popup = self.view_disk_info_popup(disk_info, font_size);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                popup,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }

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
            .style(crate::styles::subtle_tooltip);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                loading_panel,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }

        if let Some(content_preview) = &self.content_preview {
            let popup = self.view_content_preview_popup(content_preview, font_size);
            return column![
                self.build_nav_row(font_size),
                self.build_quick_nav_row(font_size),
                popup,
                self.build_status_bar(font_size),
            ]
            .spacing(2)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .into();
        }

        // ── File list ─────────────────────────────────────────────────────
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
                    items.push(rule::horizontal(1).into());
                }

                let type_label = if entry.is_dir {
                    ""
                } else {
                    get_file_icon(&entry.name)
                };

                let max_name_len = 45;
                let display_name = if entry.name.len() > max_name_len {
                    format!("{}...", &entry.name[..max_name_len - 3])
                } else {
                    entry.name.clone()
                };

                let can_show_disk_info = {
                    let lower = entry.name.to_lowercase();
                    lower.ends_with(".d64") || lower.ends_with(".d71")
                };
                let ext_for_disk = entry.name.rsplit('.').next().unwrap_or("").to_lowercase();
                let _is_disk_image = crate::file_types::is_disk_image(&ext_for_disk);
                let is_text_file = is_remote_text_file(&entry.name);
                let is_image_file = is_remote_image_file(&entry.name);
                let is_pdf_file = is_remote_pdf_file(&entry.name);

                let ext = entry.name.to_lowercase();
                let action_button: Element<'_, RemoteBrowserMessage> = if entry.is_dir {
                    Space::new().width(0).into()
                } else if ext.ends_with(".prg") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunPrg(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        "Load and run PRG file",
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                } else if ext.ends_with(".crt") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunCrt(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        "Load cartridge image",
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                } else if ext.ends_with(".sid") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlaySid(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        "Play SID music",
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                } else if ext.ends_with(".mod") || ext.ends_with(".xm") || ext.ends_with(".s3m") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlayMod(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        "Play MOD/tracker music",
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                } else if ext.ends_with(".d64")
                    || ext.ends_with(".g64")
                    || ext.ends_with(".d71")
                    || ext.ends_with(".g71")
                    || ext.ends_with(".d81")
                {
                    let mut buttons = row![].spacing(2);
                    // Show disk info button for D64/D71 only (formats we can parse)
                    if can_show_disk_info {
                        buttons = buttons.push(
                            tooltip(
                                button(text("?").size(small))
                                    .on_press(RemoteBrowserMessage::ShowDiskInfo(
                                        entry.path.clone(),
                                    ))
                                    .padding([2, 5])
                                    .style(crate::styles::action_button),
                                "Show disk directory listing",
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        );
                    }
                    let drive_str = self.selected_drive.to_drive_string();
                    let drive_label = match self.selected_drive {
                        DriveOption::A => "A",
                        DriveOption::B => "B",
                    };
                    buttons = buttons
                        .push(
                            tooltip(
                                button(text("Run").size(tiny))
                                    .on_press(RemoteBrowserMessage::RunDisk(
                                        entry.path.clone(),
                                        drive_str.clone(),
                                    ))
                                    .padding([2, 6])
                                    .style(crate::styles::action_button),
                                text(format!("Mount to Drive {} & run", drive_label)),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        )
                        .push(
                            tooltip(
                                button(text(format!("{}:RW", drive_label)).size(tiny))
                                    .on_press(RemoteBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        drive_str.clone(),
                                        "readwrite".to_string(),
                                    ))
                                    .padding([2, 4])
                                    .style(crate::styles::action_button),
                                text(format!("Mount to Drive {} (Read/Write)", drive_label)),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        )
                        .push(
                            tooltip(
                                button(text(format!("{}:RO", drive_label)).size(tiny))
                                    .on_press(RemoteBrowserMessage::MountDisk(
                                        entry.path.clone(),
                                        drive_str,
                                        "readonly".to_string(),
                                    ))
                                    .padding([2, 4])
                                    .style(crate::styles::action_button),
                                text(format!("Mount to Drive {} (Read Only)", drive_label)),
                                tooltip::Position::Top,
                            )
                            .style(crate::styles::subtle_tooltip),
                        );
                    buttons.into()
                } else if is_text_file || is_image_file || is_pdf_file {
                    tooltip(
                        button(text("View").size(small))
                            .on_press(RemoteBrowserMessage::ShowContentPreview(entry.path.clone()))
                            .padding([2, 8])
                            .style(crate::styles::action_button),
                        if is_text_file {
                            "View text content"
                        } else if is_image_file {
                            "View image"
                        } else {
                            "View PDF"
                        },
                        tooltip::Position::Top,
                    )
                    .style(crate::styles::subtle_tooltip)
                    .into()
                } else {
                    iced::widget::Space::new().width(0).into()
                };

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
                    .style(crate::styles::subtle_tooltip)
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
                    .size(fs.large)
                    .into();

                // ── Per-row rename and delete buttons ──────────────────────
                let path_for_rename = entry.path.clone();
                let path_for_delete = entry.path.clone();

                let rename_btn = tooltip(
                    button(text("Ren").size(tiny))
                        .on_press(RemoteBrowserMessage::RenameFile(path_for_rename))
                        .padding([2, 5])
                        .style(crate::styles::nav_button),
                    "Rename",
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip);

                let delete_btn = tooltip(
                    button(text("Del").size(tiny))
                        .on_press(RemoteBrowserMessage::DeleteFile(path_for_delete))
                        .padding([2, 5])
                        .style(crate::styles::nav_button),
                    if entry.is_dir {
                        "Delete directory (recursive)"
                    } else {
                        "Delete file"
                    },
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip);

                let size_text: Element<'_, RemoteBrowserMessage> = if entry.is_dir {
                    text("<DIR>").size(tiny).width(Length::Fixed(65.0)).into()
                } else {
                    text(crate::file_types::format_file_size(entry.size))
                        .size(tiny)
                        .width(Length::Fixed(65.0))
                        .into()
                };

                let file_row = row![
                    checkbox_element,
                    filename_element,
                    size_text,
                    text(type_label).size(tiny).width(Length::Fixed(35.0)),
                    action_button,
                    Space::new().width(4),
                    rename_btn,
                    delete_btn,
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .padding([2, 4]);

                items.push(file_row.into());
            }

            scrollable(
                Column::with_children(items)
                    .spacing(0)
                    .padding(iced::Padding::ZERO.right(12)),
            )
            .height(Length::Fill)
            .into()
        };

        column![
            self.build_nav_row(font_size),
            self.build_quick_nav_row(font_size),
            self.build_column_headers(font_size),
            rule::horizontal(1),
            file_list,
            rule::horizontal(1),
            self.build_status_bar(font_size),
        ]
        .spacing(2)
        .padding(5)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    // ── Delete confirm dialog ─────────────────────────────────────────────────

    fn view_delete_confirm_dialog<'a>(
        &self,
        dp: &'a DeletePending,
        font_size: u32,
    ) -> Element<'a, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;

        let header = row![
            text("⚠ Confirm Delete").size(normal),
            Space::new().width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center);

        // List up to 8 paths, then summarise the rest
        let mut path_items: Vec<Element<'_, RemoteBrowserMessage>> = dp
            .paths
            .iter()
            .take(8)
            .map(|p| {
                text(p.rsplit('/').next().unwrap_or(p))
                    .size(small)
                    .color(iced::Color::from_rgb(0.8, 0.8, 0.8))
                    .into()
            })
            .collect();
        if dp.paths.len() > 8 {
            path_items.push(
                text(format!("… and {} more", dp.paths.len() - 8))
                    .size(small)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.6))
                    .into(),
            );
        }

        let buttons = row![
            button(text("Cancel").size(normal))
                .on_press(RemoteBrowserMessage::DeleteCancel)
                .padding([6, 16])
                .style(button::secondary),
            Space::new().width(10),
            button(text("🗑 Delete").size(normal))
                .on_press(RemoteBrowserMessage::DeleteConfirm)
                .padding([6, 16])
                .style(button::secondary),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        container(
            column![
                header,
                rule::horizontal(1),
                text(&dp.summary)
                    .size(normal)
                    .color(iced::Color::from_rgb(1.0, 0.6, 0.3)),
                Column::with_children(path_items).spacing(2).padding([4, 8]),
                text("This cannot be undone.")
                    .size(small)
                    .color(iced::Color::from_rgb(0.7, 0.3, 0.3)),
                rule::horizontal(1),
                row![Space::new().width(Length::Fill), buttons].padding([4, 0]),
            ]
            .spacing(8)
            .padding(12),
        )
        .width(Length::Fill)
        .style(crate::styles::subtle_tooltip)
        .into()
    }

    // ── Rename dialog ─────────────────────────────────────────────────────────

    fn view_rename_dialog<'a>(
        &self,
        rp: &'a RenamePending,
        font_size: u32,
    ) -> Element<'a, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;
        let tiny = fs.tiny;

        let original_name = rp
            .original_path
            .rsplit('/')
            .next()
            .unwrap_or(&rp.original_path);

        let header = row![
            text("✎ Rename").size(normal),
            Space::new().width(Length::Fill),
            button(text("✖ Cancel").size(small))
                .on_press(RemoteBrowserMessage::RenameCancel)
                .padding([4, 10]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(5);

        let from_row = row![
            text("From:").size(small).width(Length::Fixed(55.0)),
            text(original_name)
                .size(normal)
                .color(iced::Color::from_rgb(0.7, 0.7, 0.7)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        let to_row = row![
            text("To:").size(small).width(Length::Fixed(55.0)),
            text_input("new name...", &rp.new_name)
                .on_input(RemoteBrowserMessage::RenameInputChanged)
                .on_submit(RemoteBrowserMessage::RenameConfirm)
                .size(normal)
                .padding(6)
                .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        let can_rename = !rp.new_name.trim().is_empty() && rp.new_name.trim() != original_name;

        let confirm_btn = if can_rename {
            button(text("✔ Rename").size(normal))
                .on_press(RemoteBrowserMessage::RenameConfirm)
                .padding([6, 16])
        } else {
            button(text("✔ Rename").size(normal)).padding([6, 16])
        };

        let hint = text("Press Enter or click Rename to confirm.")
            .size(tiny)
            .color(iced::Color::from_rgb(0.5, 0.5, 0.5));

        container(
            column![
                header,
                rule::horizontal(1),
                from_row,
                to_row,
                hint,
                rule::horizontal(1),
                row![Space::new().width(Length::Fill), confirm_btn].padding([4, 0]),
            ]
            .spacing(8)
            .padding(12),
        )
        .width(Length::Fill)
        .style(crate::styles::subtle_tooltip)
        .into()
    }

    /// Build the status bar element — shows progress bar during transfers,
    /// "Loading..." during popup loads, or the regular status message.
    fn view_status_bar(&self, small: u32) -> Element<'_, RemoteBrowserMessage> {
        if let Some(prog) = self.get_progress() {
            let file_display = if prog.current_file.len() > 30 {
                format!("...{}", &prog.current_file[prog.current_file.len() - 27..])
            } else {
                prog.current_file.clone()
            };

            if prog.total > 0 {
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
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;
        let tiny = fs.tiny;

        let header = row![
            text("💾 Create New Disk Image").size(normal),
            Space::new().width(Length::Fill),
            button(text("✖ Cancel").size(small))
                .on_press(RemoteBrowserMessage::CloseCreateDisk)
                .padding([4, 10]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(5);

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

        let dest_row = row![
            text("Dest:").size(small).width(Length::Fixed(70.0)),
            text(format!("{}/", self.current_path)).size(small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

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
        .style(crate::styles::subtle_tooltip)
        .into()
    }

    fn view_disk_info_popup(
        &self,
        disk_info: &DiskInfo,
        font_size: u32,
    ) -> Element<'_, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;
        let tiny = fs.tiny;

        let header = row![
            text(format!("{} - ", disk_info.kind)).size(small),
            text(format!("\"{}\"", disk_info.name)).size(normal),
            Space::new().width(Length::Fill),
            text(format!("{} {}", disk_info.disk_id, disk_info.dos_type)).size(small),
            Space::new().width(10),
            tooltip(
                button(text("Close").size(small))
                    .on_press(RemoteBrowserMessage::CloseDiskInfo)
                    .padding([4, 10])
                    .style(button::secondary),
                "Close directory listing",
                tooltip::Position::Left,
            )
            .style(crate::styles::subtle_tooltip),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let listing: Element<'_, RemoteBrowserMessage> =
            if let Some(png_bytes) = &self.disk_listing_image {
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

        let footer = row![
            text(format!("{} BLOCKS FREE", disk_info.blocks_free)).size(small),
            Space::new().width(Length::Fill),
            text(format!("{} files", disk_info.entries.len())).size(tiny),
        ]
        .spacing(10);

        container(
            column![
                header,
                rule::horizontal(1),
                listing,
                rule::horizontal(1),
                footer
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
    ) -> Element<'a, RemoteBrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let small = fs.small;
        let normal = fs.normal;
        let tiny = fs.tiny;

        match content {
            ContentPreview::Text {
                filename,
                content,
                line_count,
            } => {
                let display_name = if filename.len() > 40 {
                    format!("{}...", &filename[..37])
                } else {
                    filename.clone()
                };

                let header = row![
                    text("TEXT - ").size(small),
                    text(display_name).size(normal),
                    Space::new().width(Length::Fill),
                    text(format!("{} lines", line_count)).size(small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10])
                            .style(button::secondary),
                        "Close text preview",
                        tooltip::Position::Left,
                    )
                    .style(crate::styles::subtle_tooltip),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

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

                let text_content = scrollable(
                    Column::with_children(text_lines)
                        .spacing(2)
                        .padding(iced::Padding::ZERO.right(12)),
                )
                .height(Length::Fill);

                container(
                    column![header, rule::horizontal(1), text_content]
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
                let display_name = if filename.len() > 40 {
                    format!("{}...", &filename[..37])
                } else {
                    filename.clone()
                };

                let header = row![
                    text("IMAGE - ").size(small),
                    text(display_name).size(normal),
                    Space::new().width(Length::Fill),
                    text(format!("{}x{}", width, height)).size(small),
                    Space::new().width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10])
                            .style(button::secondary),
                        "Close image preview",
                        tooltip::Position::Left,
                    )
                    .style(crate::styles::subtle_tooltip),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                let image_handle = iced::widget::image::Handle::from_bytes(data.clone());
                let image_widget = iced::widget::image(image_handle)
                    .width(Length::Fill)
                    .height(Length::Fill);

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

    pub fn filter(&self) -> &str {
        &self.filter
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

    pub fn is_transferring(&self) -> bool {
        self.transfer_progress
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    fn get_progress(&self) -> Option<TransferProgress> {
        self.transfer_progress.lock().ok().and_then(|g| g.clone())
    }
}

// ─── Existing helpers (unchanged) ────────────────────────────────────────────

fn get_file_icon(name: &str) -> &'static str {
    crate::file_types::get_file_icon(name)
}

fn is_remote_text_file(name: &str) -> bool {
    crate::file_types::is_text_file(name)
}

fn is_remote_image_file(name: &str) -> bool {
    crate::file_types::is_image_file(name)
}

fn is_remote_pdf_file(name: &str) -> bool {
    crate::file_types::is_pdf_file(name)
}
