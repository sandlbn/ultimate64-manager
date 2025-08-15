use iced::{
    widget::{button, column, pick_list, row, scrollable, text, Column, Row},
    Command, Element, Length,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::{Rest, drives::MountMode};

#[derive(Debug, Clone)]
pub enum FileBrowserMessage {
    SelectDirectory,
    DirectorySelected(PathBuf),
    FileSelected(PathBuf),
    MountDisk(PathBuf, String, MountMode),
    MountCompleted(Result<(), String>),
    LoadAndRun(PathBuf),
    LoadCompleted(Result<(), String>),
    RefreshFiles,
    NavigateUp,
    DriveSelected(DriveOption),
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub extension: Option<String>,
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
    fn to_drive_string(&self) -> String {
        match self {
            DriveOption::A => "a".to_string(),
            DriveOption::B => "b".to_string(),
        }
    }

    fn get_all() -> Vec<DriveOption> {
        vec![DriveOption::A, DriveOption::B]
    }
}

pub struct FileBrowser {
    current_directory: PathBuf,
    files: Vec<FileEntry>,
    selected_file: Option<PathBuf>,
    selected_drive: DriveOption,
}

impl FileBrowser {
    pub fn new() -> Self {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut browser = Self {
            current_directory: home_dir.clone(),
            files: Vec::new(),
            selected_file: None,
            selected_drive: DriveOption::A, // Default to Drive A
        };
        browser.load_directory(&home_dir);
        browser
    }

    pub fn update(
        &mut self,
        message: FileBrowserMessage,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Command<FileBrowserMessage> {
        match message {
            FileBrowserMessage::SelectDirectory => {
                // Open native directory picker
                Command::perform(
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
                )
            }
            FileBrowserMessage::DirectorySelected(path) => {
                self.load_directory(&path);
                self.current_directory = path;
                Command::none()
            }
            FileBrowserMessage::FileSelected(path) => {
                if path.is_dir() {
                    self.load_directory(&path);
                    self.current_directory = path;
                } else {
                    self.selected_file = Some(path);
                }
                Command::none()
            }
            FileBrowserMessage::MountDisk(path, drive, mode) => {
                if let Some(conn) = connection {
                    Command::perform(
                        mount_disk_async(conn, path, drive, mode),
                        FileBrowserMessage::MountCompleted,
                    )
                } else {
                    Command::none()
                }
            }
            FileBrowserMessage::MountCompleted(result) => {
                match result {
                    Ok(_) => {
                        log::info!("Disk mounted successfully");
                        // Return a command to refresh the main status
                    }
                    Err(e) => {
                        log::error!("Mount failed: {}", e);
                    }
                }
                Command::none()
            }
            FileBrowserMessage::LoadAndRun(path) => {
                if let Some(conn) = connection {
                    Command::perform(
                        load_and_run_async(conn, path),
                        FileBrowserMessage::LoadCompleted,
                    )
                } else {
                    Command::none()
                }
            }
            FileBrowserMessage::LoadCompleted(result) => {
                if let Err(e) = result {
                    log::error!("Load failed: {}", e);
                }
                Command::none()
            }
            FileBrowserMessage::RefreshFiles => {
                self.load_directory(&self.current_directory.clone());
                Command::none()
            }
            FileBrowserMessage::NavigateUp => {
                if let Some(parent) = self.current_directory.parent() {
                    let parent_path = parent.to_path_buf();
                    self.load_directory(&parent_path);
                    self.current_directory = parent_path;
                }
                Command::none()
            }
            FileBrowserMessage::DriveSelected(drive) => {
                self.selected_drive = drive;
                Command::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, FileBrowserMessage> {
        let header = row![
            text(format!("Current: {}", self.current_directory.display())).size(16),
            button(text("Browse Folder")).on_press(FileBrowserMessage::SelectDirectory),
            button(text("Go Up")).on_press(FileBrowserMessage::NavigateUp),
            button(text("Refresh")).on_press(FileBrowserMessage::RefreshFiles),
        ]
        .spacing(10)
        .padding(5);

        // Drive selection section
        let drive_selection = row![
            text("Mount to:").size(14),
            pick_list(
                DriveOption::get_all(),
                Some(self.selected_drive.clone()),
                FileBrowserMessage::DriveSelected,
            )
            .placeholder("Select drive...")
            .width(Length::Fixed(120.0)),
        ]
        .spacing(10)
        .padding(5)
        .align_items(iced::Alignment::Center);

        let file_list: Vec<Element<'_, FileBrowserMessage>> = self
            .files
            .iter()
            .map(|entry| {
                let type_indicator = if entry.is_dir {
                    "[DIR]"
                } else {
                    match entry.extension.as_deref() {
                        Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71") => "[DSK]",
                        Some("prg") => "[PRG]",
                        Some("crt") => "[CRT]",
                        Some("sid") => "[SID]",
                        Some("mod") => "[MOD]",
                        Some("tap") => "[TAP]",
                        Some("t64") => "[T64]",
                        _ => "[FILE]",
                    }
                };

                let file_button = button(text(format!("{} {}", type_indicator, entry.name)))
                    .on_press(FileBrowserMessage::FileSelected(entry.path.clone()))
                    .width(Length::Fill);

                let mut actions = Row::new().spacing(5);

                // Add action buttons based on file type
                if !entry.is_dir {
                    match entry.extension.as_deref() {
                        Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71") => {
                            // Show which drive will be used in the button text
                            let drive_letter = match self.selected_drive {
                                DriveOption::A => "A",
                                DriveOption::B => "B",
                            };
                            
                            actions = actions
                                .push(
                                    button(text(format!("Mount {} RW", drive_letter)))
                                        .on_press(FileBrowserMessage::MountDisk(
                                            entry.path.clone(),
                                            self.selected_drive.to_drive_string(),
                                            MountMode::ReadWrite,
                                        )),
                                )
                                .push(
                                    button(text(format!("Mount {} RO", drive_letter)))
                                        .on_press(FileBrowserMessage::MountDisk(
                                            entry.path.clone(),
                                            self.selected_drive.to_drive_string(),
                                            MountMode::ReadOnly,
                                        )),
                                );
                        }
                        Some("prg") | Some("crt") => {
                            actions = actions.push(
                                button(text("Load & Run"))
                                    .on_press(FileBrowserMessage::LoadAndRun(entry.path.clone())),
                            );
                        }
                        _ => {}
                    }
                }

                row![file_button, actions]
                    .spacing(10)
                    .padding(2)
                    .into()
            })
            .collect();

        let scrollable_list = scrollable(
            Column::with_children(file_list)
                .spacing(2)
                .padding(5),
        )
        .height(Length::Fill);

        column![
            header,
            drive_selection,
            text(format!("Files ({} items)", self.files.len())).size(14),
            scrollable_list
        ]
        .spacing(10)
        .into()
    }

    fn load_directory(&mut self, path: &Path) {
        self.files.clear();
        
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut files: Vec<FileEntry> = entries
                .filter_map(|entry| {
                    entry.ok().and_then(|e| {
                        let path = e.path();
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = path.is_dir();
                        let extension = if !is_dir {
                            path.extension()
                                .and_then(|ext| ext.to_str())
                                .map(|s| s.to_lowercase())
                        } else {
                            None
                        };

                        // Filter to show only relevant files
                        if is_dir || matches!(
                            extension.as_deref(),
                            Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71") |
                            Some("prg") | Some("crt") | Some("sid") | Some("mod") | Some("tap") | Some("t64")
                        ) {
                            Some(FileEntry {
                                path,
                                name,
                                is_dir,
                                extension,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect();

            // Sort directories first, then files
            files.sort_by(|a, b| {
                match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });

            self.files = files;
        }
    }
}

async fn mount_disk_async(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    drive: String,
    mode: MountMode,
) -> Result<(), String> {
    let conn = connection.lock().await;
    
    // Mount the disk
    conn.mount_disk_image(&path, drive.clone(), mode, false)
        .map_err(|e| e.to_string())?;
    
    // If successful, log it
    log::info!("Successfully mounted {} to drive {}", path.display(), drive.to_uppercase());
    
    Ok(())
}

async fn load_and_run_async(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
) -> Result<(), String> {
    let conn = connection.lock().await;
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    
    match path.extension().and_then(|s| s.to_str()) {
        Some("crt") => conn.run_crt(&data).map_err(|e| e.to_string()),
        Some("prg") => conn.run_prg(&data).map_err(|e| e.to_string()),
        _ => Err("Unsupported file type".to_string()),
    }
}