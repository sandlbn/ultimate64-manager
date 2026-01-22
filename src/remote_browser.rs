use iced::{
    Command, Element, Length,
    widget::{
        Column, Space, button, column, container, horizontal_rule, row, scrollable, text,
        text_input, tooltip,
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;
use walkdir::WalkDir;

use crate::api;
use crate::dir_preview::{self, ContentPreview};
use crate::disk_image::{self, DiskInfo, FileType};

/// Timeout for FTP operations to prevent hangs when device goes offline
const FTP_TIMEOUT_SECS: u64 = 15;
/// Longer timeout for directory uploads which may take time
const FTP_UPLOAD_DIR_TIMEOUT_SECS: u64 = 120;

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
    // Filter
    FilterChanged(String),
    // Disk info popup
    ShowDiskInfo(String),
    DiskInfoLoaded(Result<DiskInfo, String>),
    CloseDiskInfo,
    // Content preview popup (text/image files)
    ShowContentPreview(String),
    ContentPreviewLoaded(Result<ContentPreview, String>),
    CloseContentPreview,
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
    // Disk info popup state
    disk_info_popup: Option<DiskInfo>,
    disk_info_path: Option<String>,
    disk_info_loading: bool,
    // Content preview popup state (text/image files)
    content_preview: Option<ContentPreview>,
    content_preview_path: Option<String>,
    content_preview_loading: bool,
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
            disk_info_popup: None,
            disk_info_path: None,
            disk_info_loading: false,
            content_preview: None,
            content_preview_path: None,
            content_preview_loading: false,
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
    ) -> Command<RemoteBrowserMessage> {
        match message {
            RemoteBrowserMessage::RefreshFiles => {
                if let Some(host) = &self.host_address {
                    self.is_loading = true;
                    self.status_message = Some("Loading...".to_string());
                    let path = self.current_path.clone();
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        fetch_files_ftp(host, path, password),
                        RemoteBrowserMessage::FilesLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    self.is_connected = false;
                    Command::none()
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
                Command::none()
            }

            RemoteBrowserMessage::FileSelected(path) => {
                // Check if it's a directory
                if let Some(entry) = self.files.iter().find(|f| f.path == path) {
                    if entry.is_dir {
                        self.current_path = path;
                        self.selected_file = None;
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    } else {
                        self.selected_file = Some(path);
                    }
                }
                Command::none()
            }

            RemoteBrowserMessage::NavigateUp => {
                if self.current_path != "/" {
                    if let Some(parent) = PathBuf::from(&self.current_path).parent() {
                        self.current_path = parent.to_string_lossy().to_string();
                        if self.current_path.is_empty() {
                            self.current_path = "/".to_string();
                        }
                    }
                    return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                }
                Command::none()
            }

            RemoteBrowserMessage::NavigateToPath(path) => {
                self.current_path = path;
                self.update(RemoteBrowserMessage::RefreshFiles, _connection)
            }

            RemoteBrowserMessage::DownloadFile(remote_path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Downloading...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        download_file_ftp(host, remote_path, password),
                        RemoteBrowserMessage::DownloadComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::DownloadComplete(result) => {
                match result {
                    Ok((name, _data)) => {
                        self.status_message = Some(format!("Downloaded: {}", name));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Download failed: {}", e));
                    }
                }
                Command::none()
            }

            RemoteBrowserMessage::UploadFile(local_path, remote_dest) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Uploading...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        upload_file_ftp(host, local_path, remote_dest, password),
                        RemoteBrowserMessage::UploadComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::UploadComplete(result) => {
                match result {
                    Ok(name) => {
                        self.status_message = Some(format!("Uploaded: {}", name));
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Upload failed: {}", e));
                    }
                }
                Command::none()
            }

            RemoteBrowserMessage::UploadDirectory(local_path, remote_dest) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Uploading directory...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        upload_directory_ftp(host, local_path, remote_dest, password),
                        RemoteBrowserMessage::UploadDirectoryComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::UploadDirectoryComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        return self.update(RemoteBrowserMessage::RefreshFiles, _connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Directory upload failed: {}", e));
                    }
                }
                Command::none()
            }

            RemoteBrowserMessage::RunPrg(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Running PRG...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        async move { api::run_prg(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::RunCrt(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Running CRT...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        async move { api::run_crt(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::PlaySid(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Playing SID...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        async move { api::sidplay(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::PlayMod(path) => {
                if let Some(host) = &self.host_address {
                    self.status_message = Some("Playing MOD...".to_string());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        async move { api::modplay(&host, &path, password).await },
                        RemoteBrowserMessage::RunnerComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
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
                Command::none()
            }

            RemoteBrowserMessage::MountDisk(path, drive, mode) => {
                if let Some(host) = &self.host_address {
                    self.status_message =
                        Some(format!("Mounting to drive {}...", drive.to_uppercase()));
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        async move { api::mount_disk(&host, &path, &drive, &mode, password).await },
                        RemoteBrowserMessage::MountComplete,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
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
                Command::none()
            }

            RemoteBrowserMessage::RunDisk(path, drive) => {
                if let Some(host) = &self.host_address {
                    self.status_message =
                        Some(format!("Running disk on drive {}...", drive.to_uppercase()));
                    let host = host.clone();
                    let password = self.password.clone();
                    let conn = _connection.clone();
                    Command::perform(
                        async move { api::run_disk(&host, &path, &drive, password, conn).await },
                        RemoteBrowserMessage::MountComplete, // Reuse MountComplete for result
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::FilterChanged(value) => {
                self.filter = value;
                Command::none()
            }

            // Disk info popup messages
            RemoteBrowserMessage::ShowDiskInfo(path) => {
                if let Some(host) = &self.host_address {
                    self.disk_info_loading = true;
                    self.disk_info_path = Some(path.clone());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        load_remote_disk_info(host, path, password),
                        RemoteBrowserMessage::DiskInfoLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            RemoteBrowserMessage::DiskInfoLoaded(result) => {
                self.disk_info_loading = false;
                match result {
                    Ok(info) => {
                        self.disk_info_popup = Some(info);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to read disk: {}", e));
                        self.disk_info_path = None;
                    }
                }
                Command::none()
            }

            RemoteBrowserMessage::CloseDiskInfo => {
                self.disk_info_popup = None;
                self.disk_info_path = None;
                Command::none()
            }

            // Content preview popup messages (text/image files)
            RemoteBrowserMessage::ShowContentPreview(path) => {
                if let Some(host) = &self.host_address {
                    self.content_preview_loading = true;
                    self.content_preview_path = Some(path.clone());
                    let host = host.clone();
                    let password = self.password.clone();
                    Command::perform(
                        load_remote_content_preview(host, path, password),
                        RemoteBrowserMessage::ContentPreviewLoaded,
                    )
                } else {
                    self.status_message = Some("Not connected".to_string());
                    Command::none()
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
                Command::none()
            }

            RemoteBrowserMessage::CloseContentPreview => {
                self.content_preview = None;
                self.content_preview_path = None;
                Command::none()
            }
        }
    }

    pub fn view(&self, font_size: u32) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8) as u16;
        let normal = font_size as u16;
        let tiny = (font_size.saturating_sub(3)).max(7) as u16;

        // Path display
        let display_path = if self.current_path.len() > 35 {
            format!("...{}", &self.current_path[self.current_path.len() - 32..])
        } else {
            self.current_path.clone()
        };

        // Navigation buttons with filter
        let nav_buttons = row![
            tooltip(
                button(text("Up").size(normal))
                    .on_press(RemoteBrowserMessage::NavigateUp)
                    .padding([4, 8]),
                "Go to parent folder",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            Space::with_width(Length::Fill),
            text("Filter:").size(small),
            text_input("filter...", &self.filter)
                .on_input(RemoteBrowserMessage::FilterChanged)
                .size(normal)
                .padding(4)
                .width(Length::Fixed(100.0)),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Quick navigation to common paths
        let quick_nav = row![
            tooltip(
                button(text("/").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/".to_string()))
                    .padding([2, 6]),
                "Root directory",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            tooltip(
                button(text("Usb0").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/Usb0".to_string()))
                    .padding([2, 6]),
                "USB Drive 0",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            tooltip(
                button(text("Usb1").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/Usb1".to_string()))
                    .padding([2, 6]),
                "USB Drive 1",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            tooltip(
                button(text("SD").size(small))
                    .on_press(RemoteBrowserMessage::NavigateToPath("/SD".to_string()))
                    .padding([2, 6]),
                "SD Card",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
        ]
        .spacing(3);

        // Path and status
        let path_display = text(display_path).size(normal);
        let status = if self.disk_info_loading || self.content_preview_loading {
            text("Loading...").size(small)
        } else {
            text(self.status_message.as_deref().unwrap_or("")).size(small)
        };

        // If disk info popup is open, show it instead of the file list
        if let Some(disk_info) = &self.disk_info_popup {
            let popup = self.view_disk_info_popup(disk_info, font_size);

            return column![nav_buttons, quick_nav, path_display, status, popup,]
                .spacing(5)
                .padding(5)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_items(iced::Alignment::Center)
                .into();
        }

        // If content preview popup is open, show it instead of the file list
        if let Some(content_preview) = &self.content_preview {
            let popup = self.view_content_preview_popup(content_preview, font_size);

            return column![nav_buttons, quick_nav, path_display, status, popup,]
                .spacing(5)
                .padding(5)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_items(iced::Alignment::Center)
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
                    items.push(horizontal_rule(1).into());
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
                    .style(iced::theme::Container::Box)
                    .into()
                } else if ext.ends_with(".prg") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunPrg(entry.path.clone()))
                            .padding([2, 8]),
                        "Load and run PRG file",
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
                    .into()
                } else if ext.ends_with(".crt") {
                    tooltip(
                        button(text("Run").size(small))
                            .on_press(RemoteBrowserMessage::RunCrt(entry.path.clone()))
                            .padding([2, 8]),
                        "Load cartridge image",
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
                    .into()
                } else if ext.ends_with(".sid") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlaySid(entry.path.clone()))
                            .padding([2, 8]),
                        "Play SID music",
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
                    .into()
                } else if ext.ends_with(".mod") || ext.ends_with(".xm") || ext.ends_with(".s3m") {
                    tooltip(
                        button(text("Play").size(small))
                            .on_press(RemoteBrowserMessage::PlayMod(entry.path.clone()))
                            .padding([2, 8]),
                        "Play MOD/tracker music",
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
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
                            .style(iced::theme::Container::Box),
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
                            .style(iced::theme::Container::Box),
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
                            .style(iced::theme::Container::Box),
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
                            .style(iced::theme::Container::Box),
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
                            .style(iced::theme::Container::Box),
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
                    .style(iced::theme::Container::Box)
                    .into()
                } else if is_image_file {
                    tooltip(
                        button(text("View").size(small))
                            .on_press(RemoteBrowserMessage::ShowContentPreview(entry.path.clone()))
                            .padding([2, 8]),
                        "View image",
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
                    .into()
                } else {
                    iced::widget::Space::with_width(0).into()
                };

                // Wrap filename in tooltip if truncated to show full name
                let is_truncated = entry.name.len() > max_name_len;
                let filename_button = button(text(&display_name).size(normal))
                    .on_press(RemoteBrowserMessage::FileSelected(entry.path.clone()))
                    .padding([4, 6])
                    .width(Length::Fill)
                    .style(iced::theme::Button::Text);

                let filename_element: Element<'_, RemoteBrowserMessage> = if is_truncated {
                    tooltip(
                        filename_button,
                        text(&entry.name).size(normal),
                        tooltip::Position::Top,
                    )
                    .style(iced::theme::Container::Box)
                    .into()
                } else {
                    filename_button.into()
                };

                let file_row = row![
                    // Clickable filename (with tooltip if truncated)
                    filename_element,
                    // Type label
                    text(type_label).size(tiny).width(Length::Fixed(28.0)),
                    // Action button
                    action_button,
                ]
                .spacing(4)
                .align_items(iced::Alignment::Center)
                .padding([2, 4]);

                items.push(file_row.into());
            }

            scrollable(
                Column::with_children(items)
                    .spacing(0)
                    .padding([0, 12, 0, 0]), // Right padding for scrollbar clearance
            )
            .height(Length::Fill)
            .into()
        };

        column![nav_buttons, quick_nav, path_display, status, file_list,]
            .spacing(5)
            .padding(5)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_items(iced::Alignment::Center)
            .into()
    }

    fn view_disk_info_popup(
        &self,
        disk_info: &DiskInfo,
        font_size: u32,
    ) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8) as u16;
        let normal = font_size as u16;
        let tiny = (font_size.saturating_sub(3)).max(7) as u16;

        // Header with disk name and close button
        let header = row![
            text(format!("{} - ", disk_info.kind)).size(small),
            text(format!("\"{}\"", disk_info.name)).size(normal),
            Space::with_width(Length::Fill),
            text(format!("{} {}", disk_info.disk_id, disk_info.dos_type)).size(small),
            Space::with_width(10),
            tooltip(
                button(text("Close").size(small))
                    .on_press(RemoteBrowserMessage::CloseDiskInfo)
                    .padding([4, 10]),
                "Close directory listing",
                tooltip::Position::Left,
            )
            .style(iced::theme::Container::Box),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Directory listing
        let mut listing_items: Vec<Element<'_, RemoteBrowserMessage>> = Vec::new();

        for entry in &disk_info.entries {
            let type_color = match entry.file_type {
                FileType::Prg => iced::Color::from_rgb(0.5, 0.8, 0.5),
                FileType::Seq => iced::Color::from_rgb(0.5, 0.5, 0.8),
                FileType::Rel => iced::Color::from_rgb(0.8, 0.8, 0.5),
                _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
            };

            let lock_indicator = if entry.locked { " <" } else { "" };
            let closed_indicator = if !entry.closed { "*" } else { "" };

            let entry_row = row![
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
                .style(iced::theme::Text::Color(type_color)),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center);

            listing_items.push(entry_row.into());
        }

        // Footer with blocks free
        let footer = row![
            text(format!("{} BLOCKS FREE", disk_info.blocks_free)).size(small),
            Space::with_width(Length::Fill),
            text(format!("{} files", disk_info.entries.len())).size(tiny),
        ]
        .spacing(10);

        // Scrollable listing
        let listing = scrollable(
            Column::with_children(listing_items)
                .spacing(2)
                .padding([0, 12, 0, 0]),
        )
        .height(Length::Fill);

        // Popup container with border styling
        container(
            column![
                header,
                horizontal_rule(1),
                listing,
                horizontal_rule(1),
                footer,
            ]
            .spacing(5)
            .padding(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(iced::theme::Container::Box)
        .into()
    }

    fn view_content_preview_popup(
        &self,
        content: &ContentPreview,
        font_size: u32,
    ) -> Element<'_, RemoteBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8) as u16;
        let normal = font_size as u16;
        let tiny = (font_size.saturating_sub(3)).max(7) as u16;

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
                    text(&display_name).size(normal),
                    Space::with_width(Length::Fill),
                    text(format!("{} lines", line_count)).size(small),
                    Space::with_width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10]),
                        "Close text preview",
                        tooltip::Position::Left,
                    )
                    .style(iced::theme::Container::Box),
                ]
                .spacing(5)
                .align_items(iced::Alignment::Center);

                // Text content with line numbers
                let mut text_lines: Vec<Element<'_, RemoteBrowserMessage>> = Vec::new();
                for (i, line) in content.lines().enumerate() {
                    let line_row = row![
                        text(format!("{:>4}", i + 1))
                            .size(tiny)
                            .width(Length::Fixed(35.0))
                            .style(iced::theme::Text::Color(iced::Color::from_rgb(
                                0.5, 0.5, 0.5
                            ))),
                        text(line).size(tiny),
                    ]
                    .spacing(10);
                    text_lines.push(line_row.into());
                }

                // Scrollable text content
                let text_content = scrollable(
                    Column::with_children(text_lines)
                        .spacing(2)
                        .padding([0, 12, 0, 0]),
                )
                .height(Length::Fill);

                // Popup container
                container(
                    column![header, horizontal_rule(1), text_content,]
                        .spacing(5)
                        .padding(10),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .style(iced::theme::Container::Box)
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
                    text(&display_name).size(normal),
                    Space::with_width(Length::Fill),
                    text(format!("{}x{}", width, height)).size(small),
                    Space::with_width(10),
                    tooltip(
                        button(text("Close").size(small))
                            .on_press(RemoteBrowserMessage::CloseContentPreview)
                            .padding([4, 10]),
                        "Close image preview",
                        tooltip::Position::Left,
                    )
                    .style(iced::theme::Container::Box),
                ]
                .spacing(5)
                .align_items(iced::Alignment::Center);

                // Image display using iced's image widget
                let image_handle = iced::widget::image::Handle::from_memory(data.clone());
                let image_widget = iced::widget::image(image_handle)
                    .width(Length::Fill)
                    .height(Length::Fill);

                // Popup container
                container(
                    column![
                        header,
                        horizontal_rule(1),
                        container(image_widget)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .center_x()
                            .center_y(),
                    ]
                    .spacing(5)
                    .padding(10),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .style(iced::theme::Container::Box)
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
                }
            }

            let _ = ftp.quit();

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
    let (_, data) = download_file_ftp(host, remote_path.clone(), password).await?;

    // Determine if text or image based on filename
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
    } else {
        Err("Unsupported file type for preview".to_string())
    }
}
