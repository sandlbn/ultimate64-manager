use iced::{
    Command, Element, Length,
    widget::{
        Column, Space, button, checkbox, column, horizontal_rule, pick_list, row, scrollable, text,
        text_input, tooltip,
    },
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::{Rest, drives::MountMode};

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
    FilterChanged(String),
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    #[allow(dead_code)]
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
    checked_files: HashSet<PathBuf>,
    selected_drive: DriveOption,
    status_message: Option<String>,
    filter: String,
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
        };
        browser.load_directory(&initial_dir);
        browser
    }

    pub fn update(
        &mut self,
        message: FileBrowserMessage,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Command<FileBrowserMessage> {
        match message {
            FileBrowserMessage::SelectDirectory => Command::perform(
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
                Command::none()
            }
            FileBrowserMessage::FileSelected(path) => {
                if path.is_dir() {
                    self.load_directory(&path);
                    self.current_directory = path;
                    self.checked_files.clear();
                } else {
                    self.selected_file = Some(path);
                }
                Command::none()
            }
            FileBrowserMessage::NavigateToPath(path) => {
                if path.is_dir() {
                    self.load_directory(&path);
                    self.current_directory = path;
                    self.checked_files.clear();
                }
                Command::none()
            }
            FileBrowserMessage::MountDisk(path, drive, mode) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Mounting {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Command::perform(
                        mount_disk_async(conn, path, drive, mode),
                        FileBrowserMessage::MountCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
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
                Command::none()
            }
            FileBrowserMessage::RunDisk(path, drive) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Running {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Command::perform(
                        run_disk_async(conn, path, drive),
                        FileBrowserMessage::RunDiskCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
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
                Command::none()
            }
            FileBrowserMessage::LoadAndRun(path) => {
                if let Some(conn) = connection {
                    self.status_message = Some(format!(
                        "Loading {}...",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    Command::perform(
                        load_and_run_async(conn, path),
                        FileBrowserMessage::LoadCompleted,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
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
                Command::none()
            }
            FileBrowserMessage::RefreshFiles => {
                self.load_directory(&self.current_directory.clone());
                self.status_message = None;
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
            FileBrowserMessage::ToggleFileCheck(path, checked) => {
                if checked {
                    self.checked_files.insert(path);
                } else {
                    self.checked_files.remove(&path);
                }
                Command::none()
            }
            FileBrowserMessage::SelectAll => {
                for file in &self.files {
                    self.checked_files.insert(file.path.clone());
                }
                Command::none()
            }
            FileBrowserMessage::SelectNone => {
                self.checked_files.clear();
                Command::none()
            }
            FileBrowserMessage::FilterChanged(value) => {
                self.filter = value;
                Command::none()
            }
        }
    }
    #[allow(dead_code)]
    pub fn get_selected_file(&self) -> Option<&PathBuf> {
        self.selected_file.as_ref()
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

    pub fn view(&self, font_size: u32) -> Element<'_, FileBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8) as u16;
        let normal = font_size as u16;
        let tiny = (font_size.saturating_sub(3)).max(7) as u16;

        // Current path display (truncated if too long)
        let path_str = self.current_directory.to_string_lossy();
        let display_path = if path_str.len() > 40 {
            format!("...{}", &path_str[path_str.len() - 37..])
        } else {
            path_str.to_string()
        };

        // Navigation buttons with filter
        let nav_buttons = row![
            tooltip(
                button(text("Up").size(normal))
                    .on_press(FileBrowserMessage::NavigateUp)
                    .padding([4, 8]),
                "Go to parent folder",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            tooltip(
                button(text("Browse").size(normal))
                    .on_press(FileBrowserMessage::SelectDirectory)
                    .padding([4, 8]),
                "Choose a different folder",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            Space::with_width(Length::Fill),
            text("Filter:").size(small),
            text_input("filter...", &self.filter)
                .on_input(FileBrowserMessage::FilterChanged)
                .size(normal)
                .padding(4)
                .width(Length::Fixed(100.0)),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Path display
        let path_display = text(display_path).size(normal);

        // Drive selection and selection controls
        let controls_row = row![
            text("Mount:").size(small),
            tooltip(
                pick_list(
                    DriveOption::get_all(),
                    Some(self.selected_drive.clone()),
                    FileBrowserMessage::DriveSelected,
                )
                .placeholder("Drive")
                .text_size(normal)
                .width(Length::Fixed(95.0)),
                "Select target drive for mounting disks",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            Space::with_width(10),
            tooltip(
                button(text("All").size(tiny))
                    .on_press(FileBrowserMessage::SelectAll)
                    .padding([2, 6]),
                "Select all files",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            tooltip(
                button(text("None").size(tiny))
                    .on_press(FileBrowserMessage::SelectNone)
                    .padding([2, 6]),
                "Deselect all files",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
            Space::with_width(Length::Fill),
            text(format!("{} files", self.files.len())).size(small),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Checked count
        let checked_count = self.checked_files.len();
        let selection_info = if checked_count > 0 {
            text(format!("{} selected", checked_count)).size(small)
        } else {
            text("").size(small)
        };

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
                file_list.push(horizontal_rule(1).into());
            }
            file_list.push(self.view_file_entry(*entry, font_size));
        }

        let scrollable_list = scrollable(
            Column::with_children(file_list)
                .spacing(0)
                .padding([0, 12, 0, 0]), // Right padding for scrollbar clearance
        )
        .height(Length::Fill);

        // Status message
        let status = if let Some(msg) = &self.status_message {
            text(msg).size(small)
        } else {
            text("").size(small)
        };

        column![
            path_display,
            nav_buttons,
            controls_row,
            selection_info,
            scrollable_list,
            status,
        ]
        .spacing(3)
        .padding(5)
        .into()
    }

    fn view_file_entry(
        &self,
        entry: &FileEntry,
        font_size: u32,
    ) -> Element<'_, FileBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8) as u16;
        let normal = font_size as u16;
        let tiny = (font_size.saturating_sub(3)).max(7) as u16;

        let is_checked = self.checked_files.contains(&entry.path);

        // File type label
        let type_label = if entry.is_dir {
            ""
        } else {
            match entry.extension.as_deref() {
                Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71") => "DSK",
                Some("prg") => "PRG",
                Some("crt") => "CRT",
                Some("sid") => "SID",
                Some("mod") => "MOD",
                Some("tap") | Some("t64") => "TAP",
                _ => "",
            }
        };

        // Truncate long filenames
        let max_name_len = 32;
        let display_name = if entry.name.len() > max_name_len {
            format!("{}...", &entry.name[..max_name_len - 3])
        } else {
            entry.name.clone()
        };

        // Action button based on file type
        let action_button: Element<'_, FileBrowserMessage> = if entry.is_dir {
            // Directory - click to enter
            tooltip(
                button(text("Open").size(small))
                    .on_press(FileBrowserMessage::FileSelected(entry.path.clone()))
                    .padding([2, 8]),
                "Open folder",
                tooltip::Position::Top,
            )
            .style(iced::theme::Container::Box)
            .into()
        } else {
            match entry.extension.as_deref() {
                Some("d64") | Some("d71") | Some("d81") | Some("g64") | Some("g71") => {
                    let drive = match self.selected_drive {
                        DriveOption::A => "A",
                        DriveOption::B => "B",
                    };
                    let drive_num = match self.selected_drive {
                        DriveOption::A => "8",
                        DriveOption::B => "9",
                    };
                    row![
                        tooltip(
                            button(text("Run").size(small))
                                .on_press(FileBrowserMessage::RunDisk(
                                    entry.path.clone(),
                                    self.selected_drive.to_drive_string(),
                                ))
                                .padding([2, 5]),
                            text(format!("Mount, reset and LOAD\"*\",{},1 + RUN", drive_num))
                                .size(normal),
                            tooltip::Position::Top,
                        )
                        .style(iced::theme::Container::Box),
                        tooltip(
                            button(text(format!("{}:RW", drive)).size(small))
                                .on_press(FileBrowserMessage::MountDisk(
                                    entry.path.clone(),
                                    self.selected_drive.to_drive_string(),
                                    MountMode::ReadWrite,
                                ))
                                .padding([2, 5]),
                            text(format!("Mount as Drive {} (Read/Write)", drive_num)).size(normal),
                            tooltip::Position::Top,
                        )
                        .style(iced::theme::Container::Box),
                        tooltip(
                            button(text(format!("{}:RO", drive)).size(small))
                                .on_press(FileBrowserMessage::MountDisk(
                                    entry.path.clone(),
                                    self.selected_drive.to_drive_string(),
                                    MountMode::ReadOnly,
                                ))
                                .padding([2, 5]),
                            text(format!("Mount as Drive {} (Read Only)", drive_num)).size(normal),
                            tooltip::Position::Top,
                        )
                        .style(iced::theme::Container::Box),
                    ]
                    .spacing(2)
                    .into()
                }
                Some("prg") | Some("crt") => tooltip(
                    button(text("Run").size(small))
                        .on_press(FileBrowserMessage::LoadAndRun(entry.path.clone()))
                        .padding([2, 10]),
                    "Load and run on Ultimate64",
                    tooltip::Position::Top,
                )
                .style(iced::theme::Container::Box)
                .into(),
                _ => Space::with_width(0).into(),
            }
        };

        // Build the row: [checkbox] [name...] [type] [action]
        let path_clone = entry.path.clone();
        let checkbox_element: Element<'_, FileBrowserMessage> = checkbox("", is_checked)
            .on_toggle(move |checked| {
                FileBrowserMessage::ToggleFileCheck(path_clone.clone(), checked)
            })
            .size(16)
            .into();

        let file_row = row![
            // Checkbox (only for files, not dirs)
            checkbox_element,
            // Clickable filename (truncated)
            button(text(&display_name).size(normal))
                .on_press(FileBrowserMessage::FileSelected(entry.path.clone()))
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

        file_row.into()
    }

    fn load_directory(&mut self, path: &Path) {
        self.files.clear();

        if let Ok(entries) = std::fs::read_dir(path) {
            let mut files: Vec<FileEntry> = entries
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

                        // Filter: show directories and relevant file types
                        if is_dir
                            || matches!(
                                extension.as_deref(),
                                Some("d64")
                                    | Some("d71")
                                    | Some("d81")
                                    | Some("g64")
                                    | Some("g71")
                                    | Some("prg")
                                    | Some("crt")
                                    | Some("sid")
                                    | Some("mod")
                                    | Some("tap")
                                    | Some("t64")
                            )
                        {
                            Some(FileEntry {
                                path,
                                name,
                                is_dir,
                                extension,
                                size,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect();

            // Sort: directories first, then alphabetically
            files.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            self.files = files;
        }

        log::debug!("Loaded {} items from {}", self.files.len(), path.display());
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
    let result = tokio::task::spawn_blocking(move || {
        let conn = connection.blocking_lock();
        conn.mount_disk_image(&path, drive.clone(), mode, false)
            .map_err(|e| {
                log::error!("Mount error: {}", e);
                e.to_string()
            })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    if result.is_ok() {
        log::info!("Mount successful");
    }
    result
}

async fn run_disk_async(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    drive: String,
) -> Result<(), String> {
    log::info!("Running disk {} on drive {}", path.display(), drive);

    // Determine device number based on drive
    let device_num = if drive == "a" { "8" } else { "9" };

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
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
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
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}
