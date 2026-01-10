use iced::{
    Command, Element, Length,
    widget::{
        Column, Space, button, column, horizontal_rule, row, scrollable, text, text_input, tooltip,
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::api;

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
        let status = text(self.status_message.as_deref().unwrap_or("")).size(small);

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

                let is_selected = self.selected_file.as_ref() == Some(&entry.path);

                // File type label
                let type_label = if entry.is_dir {
                    ""
                } else {
                    get_file_icon(&entry.name)
                };

                // Truncate long filenames
                let max_name_len = 28;
                let display_name = if entry.name.len() > max_name_len {
                    format!("{}...", &entry.name[..max_name_len - 3])
                } else {
                    entry.name.clone()
                };

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
                    row![
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
                    ]
                    .spacing(2)
                    .into()
                } else {
                    iced::widget::Space::with_width(0).into()
                };

                let file_row = row![
                    // Clickable filename
                    button(text(&display_name).size(normal))
                        .on_press(RemoteBrowserMessage::FileSelected(entry.path.clone()))
                        .padding([4, 6])
                        .width(Length::Fill)
                        .style(iced::theme::Button::Text),
                    // Type label
                    text(type_label).size(tiny).width(Length::Fixed(28.0)),
                    // Action button
                    action_button,
                ]
                .spacing(4)
                .align_items(iced::Alignment::Center)
                .padding([2, 4]);

                if is_selected {
                    items.push(column![file_row].width(Length::Fill).into());
                } else {
                    items.push(file_row.into());
                }
            }

            scrollable(
                Column::with_children(items)
                    .spacing(0)
                    .width(Length::Fill)
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
    } else if lower.ends_with(".txt") || lower.ends_with(".nfo") {
        "TXT"
    } else {
        ""
    }
}

// Fetch files via FTP
async fn fetch_files_ftp(
    host: String,
    path: String,
    password: Option<String>,
) -> Result<Vec<RemoteFileEntry>, String> {
    log::info!("FTP: Listing {} on {}", path, host);

    let result = tokio::task::spawn_blocking(move || {
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
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?;

    result
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

    let result = tokio::task::spawn_blocking(move || {
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
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?;

    result
}

// Upload file via FTP
async fn upload_file_ftp(
    host: String,
    local_path: PathBuf,
    remote_dest: String,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("FTP: Uploading {} to {}", local_path.display(), remote_dest);

    let result = tokio::task::spawn_blocking(move || {
        use std::io::Cursor;
        use std::time::Duration;
        use suppaftp::FtpStream;

        // Read local file
        let data = std::fs::read(&local_path).map_err(|e| format!("Cannot read file: {}", e))?;

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
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?;

    result
}
