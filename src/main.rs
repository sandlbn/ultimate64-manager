// Remove the windows_subsystem attribute during development to see console output
// Uncomment for release builds:
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use iced::{
    Element, Length, Subscription, Task, Theme,
    widget::{
        Space, button, column, container, pick_list, row, rule, scrollable, text, text_input,
        tooltip,
    },
    window,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;
use url::Host;
use version_check::{NewVersionInfo, VersionCheckMessage};

mod api;
mod config_editor;
mod config_presets;
mod csdb;
mod csdb_browser;
mod dir_preview;
mod discovery;
mod disk_image;
mod file_browser;
mod memory_editor;
mod mod_info;
mod music_player;
mod pdf_preview;
mod petscii;
mod profiles;
mod remote_browser;
mod settings;
mod stream_control;
mod streaming;
mod templates;
mod version_check;
mod video_scaling;

use config_editor::{ConfigEditor, ConfigEditorMessage};
use csdb_browser::{CsdbBrowser, CsdbBrowserMessage};
use discovery::DiscoveredDevice;
use file_browser::{FileBrowser, FileBrowserMessage};
use memory_editor::{MemoryEditor, MemoryEditorMessage};
use music_player::{MusicPlayer, MusicPlayerMessage, PlaybackState};
use profiles::ProfileManager;
use remote_browser::{RemoteBrowser, RemoteBrowserMessage};
use settings::{AppSettings, ConnectionSettings, StreamControlMethod};
use streaming::{StreamingMessage, VideoStreaming};
use templates::{DiskTemplate, TemplateManager};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn main() -> iced::Result {
    // Force OpenGL backend on Linux for better compatibility with multi-GPU systems
    // Users can override with WGPU_BACKEND=vulkan if needed
    #[cfg(target_os = "linux")]
    if std::env::var("WGPU_BACKEND").is_err() {
        // SAFETY: This is executed at program startup before any threads
        // or GPU backends are initialized. No other threads can read
        // environment variables at this point.
        unsafe {
            std::env::set_var("WGPU_BACKEND", "gl");
        }
    }

    // Initialize logger - show info level by default, debug if RUST_LOG is set
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    log::info!("===========================================");
    log::info!("Starting Ultimate64 Manager v{}", APP_VERSION);
    log::info!("===========================================");

    // Print some diagnostic info
    log::info!("Platform: {}", std::env::consts::OS);
    log::info!("Arch: {}", std::env::consts::ARCH);

    iced::daemon(
        Ultimate64Browser::new,
        Ultimate64Browser::update,
        Ultimate64Browser::view,
    )
    .title(Ultimate64Browser::title)
    .subscription(Ultimate64Browser::subscription)
    .theme(Ultimate64Browser::theme)
    .run()
}

fn load_window_icon() -> Option<iced::window::Icon> {
    // Embedded icon - icon.png MUST exist in icons/ folder at compile time
    // If you don't have icons/icon.png, compilation will fail
    const ICON_BYTES: &[u8] = include_bytes!("../icons/icon.png");

    if let Ok(img) = image::load_from_memory(ICON_BYTES) {
        let img = img.resize_exact(32, 32, image::imageops::FilterType::Lanczos3);
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        if let Ok(icon) = iced::window::icon::from_rgba(rgba.into_raw(), width, height) {
            log::info!("Using embedded icon (32x32)");
            return Some(icon);
        }
    }

    log::warn!("Failed to load embedded icon, using fallback");

    // Fallback: create a simple colored icon programmatically
    let size = 32u32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let border = x < 2 || x >= size - 2 || y < 2 || y >= size - 2;
            if border {
                pixels.extend_from_slice(&[134, 122, 222, 255]); // C64 light blue
            } else {
                pixels.extend_from_slice(&[64, 50, 133, 255]); // C64 blue
            }
        }
    }
    iced::window::icon::from_rgba(pixels, size, size).ok()
}

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    TabSelected(Tab),

    // File browsers (dual pane)
    LeftBrowser(FileBrowserMessage),
    RemoteBrowser(RemoteBrowserMessage),
    ActivePaneChanged(Pane),

    // Copy operations
    CopyLocalToRemote, // Copy selected local file to Ultimate64
    CopyRemoteToLocal, // Copy selected remote file to local
    CopyComplete(Result<String, String>),

    // Music player
    MusicPlayer(MusicPlayerMessage),

    // Configuration editor
    ConfigEditor(ConfigEditorMessage),

    // Connection
    HostInputChanged(String),
    PasswordInputChanged(String),
    ConnectPressed,
    DisconnectPressed,
    RefreshStatus,
    RefreshAfterConnect,
    StatusUpdated(Result<StatusInfo, String>),
    StreamControlMethodChanged(StreamControlMethod),

    // Templates
    TemplateSelected(DiskTemplate),
    ExecuteTemplate,

    // Errors
    ShowError(String),
    ShowInfo(String),
    DismissMessage,

    // Video/Streaming
    Streaming(StreamingMessage),
    ExitFullscreen,

    // Memory Editor
    MemoryEditor(MemoryEditorMessage),
    // Machine control
    ResetMachine,
    RebootMachine,
    PauseMachine,
    ResumeMachine,
    PoweroffMachine,
    MachineCommandCompleted(Result<String, String>),

    // Settings
    DefaultSongDurationChanged(String),
    FontSizeChanged(String),

    // Starting directory settings
    BrowseFileBrowserStartDir,
    FileBrowserStartDirSelected(Option<PathBuf>),
    ClearFileBrowserStartDir,
    BrowseMusicPlayerStartDir,
    MusicPlayerStartDirSelected(Option<PathBuf>),
    ClearMusicPlayerStartDir,
    // Version check
    VersionCheck(VersionCheckMessage),
    OpenReleasePage,
    // CSDb Browser
    CsdbBrowser(CsdbBrowserMessage),
    // Separate streaming window management
    OpenStreamingWindow,
    StreamingWindowOpened(iced::window::Id),
    WindowClosed(iced::window::Id),
    CloseStreamingWindow,
    // Profile management
    ProfileSelected(String),
    NewProfileNameChanged(String),
    CreateProfile,
    DuplicateProfile,
    DeleteProfile,
    RenameProfile,
    RenameProfileNameChanged(String),
    // Discovery
    StartDiscovery,
    DiscoveryComplete(Vec<discovery::DiscoveredDevice>),
    SelectDiscoveredDevice(discovery::DiscoveredDevice),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    DualPaneBrowser,
    MusicPlayer,
    VideoViewer,
    MemoryEditor,
    Configuration,
    CsdbBrowser,
    Settings,
}

impl std::fmt::Display for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tab::DualPaneBrowser => write!(f, "File Browser"),
            Tab::MusicPlayer => write!(f, "Music Player"),
            Tab::VideoViewer => write!(f, "Video Viewer"),
            Tab::MemoryEditor => write!(f, "Memory Editor"),
            Tab::Configuration => write!(f, "Configuration"),
            Tab::CsdbBrowser => write!(f, "CSDb"),
            Tab::Settings => write!(f, "Settings"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub connected: bool,
    pub device_info: Option<String>,
    pub mounted_disks: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum UserMessage {
    Error(String),
    Info(String),
}

pub struct Ultimate64Browser {
    active_tab: Tab,

    // Dual-pane file browsers
    left_browser: FileBrowser,
    remote_browser: RemoteBrowser,
    active_pane: Pane,

    music_player: MusicPlayer,
    memory_editor: MemoryEditor,
    config_editor: ConfigEditor,
    settings: AppSettings,
    template_manager: TemplateManager,
    selected_template: Option<DiskTemplate>,
    connection: Option<Arc<TokioMutex<Rest>>>,
    host_url: Option<String>, // Store host URL for direct HTTP requests
    status: StatusInfo,

    // User messages (errors and info)
    user_message: Option<UserMessage>,

    // Input fields
    host_input: String,
    password_input: String,
    font_size_input: String,

    // Video streaming
    video_streaming: VideoStreaming,
    // Update notification
    new_version: Option<NewVersionInfo>,
    csdb_browser: CsdbBrowser,
    // Separate streaming window
    streaming_window_id: Option<window::Id>,
    main_window_id: Option<window::Id>,

    // Profile management
    profile_manager: ProfileManager,
    new_profile_name: String,
    rename_profile_name: String,
    // Device discovery
    is_discovering: bool,
    discovered_devices: Vec<DiscoveredDevice>,
}

impl Ultimate64Browser {
    fn new() -> (Self, Task<Message>) {
        log::info!("Initializing application...");

        let profile_manager = ProfileManager::load();
        let settings = profile_manager.active_settings().clone();
        log::info!("Active profile: {}", profile_manager.active_profile);

        // Create music player with configured starting directory
        let mut music_player =
            MusicPlayer::new(settings.default_paths.music_player_start_dir.clone());
        music_player.set_default_song_duration(settings.preferences.default_song_duration);

        // Create file browser with configured starting directory
        let left_browser = FileBrowser::new(settings.default_paths.file_browser_start_dir.clone());
        let settings = match AppSettings::load() {
            Ok(s) => {
                log::info!("Loaded settings from config file");
                s
            }
            Err(e) => {
                log::warn!("Could not load settings: {}. Using defaults.", e);
                AppSettings::default()
            }
        };
        // Load window icon
        let icon = load_window_icon();

        // Open main window
        let (main_window_id, open_main_window) = iced::window::open(iced::window::Settings {
            size: iced::Size::new(1200.0, 800.0),
            min_size: Some(iced::Size::new(800.0, 600.0)),
            icon: icon,
            ..Default::default()
        });

        let mut app = Self {
            active_tab: Tab::DualPaneBrowser,
            left_browser,
            remote_browser: RemoteBrowser::new(),
            active_pane: Pane::Left,
            music_player,
            memory_editor: MemoryEditor::new(),
            config_editor: ConfigEditor::new(),
            host_input: settings.connection.host.clone(),
            password_input: settings.connection.password.clone().unwrap_or_default(),
            font_size_input: settings.preferences.font_size.to_string(),
            settings: settings.clone(),
            host_url: None,
            template_manager: TemplateManager::new(),
            selected_template: None,
            connection: None,
            new_version: None,
            status: StatusInfo {
                connected: false,
                device_info: None,
                mounted_disks: Vec::new(),
            },
            user_message: None,
            video_streaming: VideoStreaming::new(),
            csdb_browser: CsdbBrowser::new(),
            main_window_id: Some(main_window_id),
            streaming_window_id: None,
            profile_manager,
            new_profile_name: String::new(),
            rename_profile_name: String::new(),
            is_discovering: false,
            discovered_devices: Vec::new(),
        };
        app.video_streaming
            .set_stream_control_method(settings.connection.stream_control_method);

        // Check for updates on startup
        let version_check_cmd =
            version_check::check_for_updates(APP_VERSION).map(Message::VersionCheck);

        // Build init tasks
        let mut init_tasks = vec![
            open_main_window.map(|_| Message::RefreshStatus),
            version_check_cmd,
        ];

        // Auto-connect if host is configured
        if !settings.connection.host.is_empty() {
            log::info!(
                "Auto-connecting to configured host: {}",
                settings.connection.host
            );
            app.establish_connection();
            init_tasks.push(Task::perform(
                async {
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                },
                |_| Message::RefreshStatus,
            ));
        } else {
            log::info!("No host configured, waiting for user input");
        }

        (app, Task::batch(init_tasks))
    }

    fn title(&self, window_id: iced::window::Id) -> String {
        if Some(window_id) == self.streaming_window_id {
            return "Ultimate64 Video Stream".to_string();
        }
        if Some(window_id) == self.main_window_id {
            // Main window title
            let connection_status = if self.status.connected {
                format!(" - Connected to {}", self.settings.connection.host)
            } else {
                " - Disconnected".to_string()
            };
            return format!("Ultimate64 Manager v{}{}", APP_VERSION, connection_status);
        }
        // Fallback
        "Ultimate64 Manager".to_string()
    }
    fn theme(&self, _window_id: iced::window::Id) -> Theme {
        // Custom dark theme with lighter blue (like the reference screenshot)
        Theme::custom(
            "Ultimate64 Dark".to_string(),
            iced::theme::Palette {
                background: iced::Color::from_rgb(0.15, 0.15, 0.18), // Dark background
                text: iced::Color::from_rgb(0.9, 0.9, 0.9),          // Light text
                primary: iced::Color::from_rgb(0.45, 0.52, 0.85),    // Lighter blue
                success: iced::Color::from_rgb(0.3, 0.7, 0.3),       // Green
                danger: iced::Color::from_rgb(0.8, 0.3, 0.3),        // Red
                warning: iced::Color::from_rgb(0.9, 0.7, 0.2),       // Yellow/Orange
            },
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::StartDiscovery => {
                if self.is_discovering {
                    return Task::none();
                }
                self.is_discovering = true;
                self.discovered_devices.clear();
                self.user_message = Some(UserMessage::Info("Scanning network...".to_string()));

                Task::perform(discovery::discover_devices(), Message::DiscoveryComplete)
            }

            Message::DiscoveryComplete(devices) => {
                self.is_discovering = false;
                self.discovered_devices = devices.clone();

                if devices.is_empty() {
                    self.user_message = Some(UserMessage::Info(
                        "No Ultimate devices found on network".to_string(),
                    ));
                } else {
                    self.user_message = Some(UserMessage::Info(format!(
                        "Found {} device(s)",
                        devices.len()
                    )));
                }
                Task::none()
            }

            Message::SelectDiscoveredDevice(device) => {
                self.host_input = device.ip.clone();
                self.user_message = Some(UserMessage::Info(format!(
                    "Selected: {} ({})",
                    device.product, device.ip
                )));
                Task::none()
            }
            Message::TabSelected(tab) => {
                log::debug!("Tab selected: {:?}", tab);
                self.active_tab = tab;
                Task::none()
            }
            Message::CloseStreamingWindow => {
                if let Some(id) = self.streaming_window_id {
                    self.streaming_window_id = None;
                    return iced::window::close(id);
                }
                Task::none()
            }
            Message::MemoryEditor(msg) => self
                .memory_editor
                .update(msg, self.connection.clone())
                .map(Message::MemoryEditor),
            Message::LeftBrowser(msg) => {
                // Check if this is a "run" operation that should stop music
                let should_stop_music = matches!(
                    &msg,
                    FileBrowserMessage::RunDisk(_, _) | FileBrowserMessage::LoadAndRun(_)
                ) && self.music_player.playback_state
                    == PlaybackState::Playing;

                let should_refresh = matches!(msg, FileBrowserMessage::MountCompleted(Ok(_)));

                let cmd = self
                    .left_browser
                    .update(msg, self.connection.clone())
                    .map(Message::LeftBrowser);

                let mut commands = vec![cmd];

                // Stop music player if running a cartridge/disk
                if should_stop_music {
                    log::info!("Stopping music player - running cartridge/disk");
                    commands.push(
                        self.music_player
                            .update(MusicPlayerMessage::Stop, self.connection.clone())
                            .map(Message::MusicPlayer),
                    );
                }

                if should_refresh {
                    commands.push(Task::perform(
                        async {
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        },
                        |_| Message::RefreshStatus,
                    ));
                }

                Task::batch(commands)
            }

            Message::RemoteBrowser(msg) => {
                // Check if this is a "run" operation that should stop music
                let should_stop_music = matches!(
                    &msg,
                    RemoteBrowserMessage::RunPrg(_)
                        | RemoteBrowserMessage::RunCrt(_)
                        | RemoteBrowserMessage::PlaySid(_)
                        | RemoteBrowserMessage::PlayMod(_)
                        | RemoteBrowserMessage::RunDisk(_, _)
                ) && self.music_player.playback_state
                    == PlaybackState::Playing;

                // Intercept UploadDirectoryComplete to show result in main status bar
                if let RemoteBrowserMessage::UploadDirectoryComplete(ref result) = msg {
                    match result {
                        Ok(m) => {
                            self.user_message = Some(UserMessage::Info(m.clone()));
                        }
                        Err(e) => {
                            self.user_message = Some(UserMessage::Error(e.clone()));
                        }
                    }
                }

                let cmd = self
                    .remote_browser
                    .update(msg, self.connection.clone())
                    .map(Message::RemoteBrowser);

                if should_stop_music {
                    log::info!("Stopping music player - running file from Ultimate64");
                    Task::batch(vec![
                        cmd,
                        self.music_player
                            .update(MusicPlayerMessage::Stop, self.connection.clone())
                            .map(Message::MusicPlayer),
                    ])
                } else {
                    cmd
                }
            }
            Message::ActivePaneChanged(pane) => {
                self.active_pane = pane;
                Task::none()
            }

            Message::CopyLocalToRemote => {
                // Copy checked local files and directories to Ultimate64
                let items_to_copy = self.left_browser.get_checked_files();

                if items_to_copy.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "No files selected. Use checkboxes to select files.".to_string(),
                    ));
                    return Task::none();
                }

                // Separate files and directories
                let files_to_copy: Vec<PathBuf> = items_to_copy
                    .iter()
                    .filter(|p| !p.is_dir())
                    .cloned()
                    .collect();
                let dirs_to_copy: Vec<PathBuf> = items_to_copy
                    .iter()
                    .filter(|p| p.is_dir())
                    .cloned()
                    .collect();

                if files_to_copy.is_empty() && dirs_to_copy.is_empty() {
                    self.user_message =
                        Some(UserMessage::Error("No valid items selected.".to_string()));
                    return Task::none();
                }

                let remote_dest = self.remote_browser.get_current_path().to_string();

                // Build commands for each operation
                let mut commands: Vec<Task<Message>> = Vec::new();

                // Handle directories via RemoteBrowser's UploadDirectory
                for dir_path in dirs_to_copy {
                    commands.push(
                        self.remote_browser
                            .update(
                                RemoteBrowserMessage::UploadDirectory(
                                    dir_path,
                                    remote_dest.clone(),
                                ),
                                self.connection.clone(),
                            )
                            .map(Message::RemoteBrowser),
                    );
                }

                // Handle files with batch upload
                if !files_to_copy.is_empty() {
                    if let Some(host) = &self.host_url {
                        let host = host
                            .trim_start_matches("http://")
                            .trim_start_matches("https://")
                            .to_string();
                        let file_count = files_to_copy.len();
                        let password = self.settings.connection.password.clone();
                        let remote_dest_clone = remote_dest.clone();

                        self.user_message = Some(UserMessage::Info(format!(
                            "Uploading {} file(s) and {} folder(s) via FTP...",
                            file_count,
                            commands.len()
                        )));

                        commands.push(Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    use std::io::Cursor;
                                    use std::path::PathBuf;
                                    use std::time::Duration;
                                    use suppaftp::FtpStream;

                                    let addr = format!("{}:21", host);
                                    let mut ftp = FtpStream::connect(&addr)
                                        .map_err(|e| format!("FTP connect failed: {}", e))?;

                                    ftp.get_ref()
                                        .set_write_timeout(Some(Duration::from_secs(120)))
                                        .ok();

                                    // Login with configured password or anonymous
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
                                        .map_err(|e| format!("Set binary mode failed: {}", e))?;

                                    ftp.cwd(&remote_dest_clone).map_err(|e| {
                                        format!("Cannot access {}: {}", remote_dest_clone, e)
                                    })?;

                                    let mut uploaded = 0;
                                    for local_path in &files_to_copy {
                                        let local_path: &PathBuf = local_path;
                                        let data =
                                            std::fs::read(local_path.as_path()).map_err(|e| {
                                                format!(
                                                    "Cannot read {}: {}",
                                                    local_path.display(),
                                                    e
                                                )
                                            })?;

                                        let filename = local_path
                                            .file_name()
                                            .and_then(|n: &std::ffi::OsStr| n.to_str())
                                            .unwrap_or("file")
                                            .to_string();

                                        let mut cursor = Cursor::new(data);
                                        ftp.put_file(&filename, &mut cursor).map_err(|e| {
                                            format!("FTP upload {} failed: {}", filename, e)
                                        })?;

                                        uploaded += 1;
                                    }

                                    let _ = ftp.quit();

                                    Ok(format!("Uploaded {} file(s)", uploaded))
                                })
                                .await
                                .map_err(|e| e.to_string())?
                            },
                            Message::CopyComplete,
                        ));
                    } else {
                        self.user_message = Some(UserMessage::Error(
                            "Not connected to Ultimate64".to_string(),
                        ));
                        return Task::none();
                    }
                } else {
                    // Only directories being uploaded
                    self.user_message = Some(UserMessage::Info(format!(
                        "Uploading {} folder(s) via FTP...",
                        commands.len()
                    )));
                }

                if commands.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "Not connected to Ultimate64".to_string(),
                    ));
                    return Task::none();
                }

                return Task::batch(commands);
            }
            Message::CopyRemoteToLocal => {
                // Copy selected remote file to local directory
                if let Some(remote_path) = self.remote_browser.get_selected_file() {
                    if let Some(host) = &self.host_url {
                        let host = host
                            .trim_start_matches("http://")
                            .trim_start_matches("https://")
                            .to_string();
                        let remote_path = remote_path.to_string();
                        let local_dest = self.left_browser.get_current_directory().clone();
                        let password = self.settings.connection.password.clone();

                        self.user_message =
                            Some(UserMessage::Info("Downloading file via FTP...".to_string()));

                        return Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    use std::io::Read;
                                    use std::time::Duration;
                                    use suppaftp::FtpStream;

                                    let addr = format!("{}:21", host);
                                    let mut ftp = FtpStream::connect(&addr)
                                        .map_err(|e| format!("FTP connect failed: {}", e))?;

                                    ftp.get_ref()
                                        .set_read_timeout(Some(Duration::from_secs(60)))
                                        .ok();

                                    // Login with configured password or anonymous
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
                                        .map_err(|e| format!("Set binary mode failed: {}", e))?;

                                    let mut reader = ftp
                                        .retr_as_stream(&remote_path)
                                        .map_err(|e| format!("FTP download failed: {}", e))?;

                                    let mut data = Vec::new();
                                    reader
                                        .read_to_end(&mut data)
                                        .map_err(|e| format!("Read error: {}", e))?;

                                    ftp.finalize_retr_stream(reader)
                                        .map_err(|e| format!("Transfer finalize error: {}", e))?;

                                    let _ = ftp.quit();

                                    let filename = remote_path.rsplit('/').next().unwrap_or("file");
                                    let local_path = local_dest.join(filename);

                                    std::fs::write(&local_path, &data)
                                        .map_err(|e| format!("Write error: {}", e))?;

                                    Ok(format!("Downloaded: {} ({} bytes)", filename, data.len()))
                                })
                                .await
                                .map_err(|e| e.to_string())?
                            },
                            Message::CopyComplete,
                        );
                    } else {
                        self.user_message = Some(UserMessage::Error(
                            "Not connected to Ultimate64".to_string(),
                        ));
                    }
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "No file selected in remote pane".to_string(),
                    ));
                }
                Task::none()
            }

            Message::CopyComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.user_message = Some(UserMessage::Info(msg));
                        // Clear checked files after successful copy
                        self.left_browser.clear_checked();
                        // Refresh both browsers
                        return Task::batch(vec![
                            self.left_browser
                                .update(FileBrowserMessage::RefreshFiles, self.connection.clone())
                                .map(Message::LeftBrowser),
                            self.remote_browser
                                .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                                .map(Message::RemoteBrowser),
                        ]);
                    }
                    Err(e) => {
                        self.user_message = Some(UserMessage::Error(e));
                    }
                }
                Task::none()
            }
            Message::VersionCheck(msg) => {
                match msg {
                    VersionCheckMessage::CheckComplete(result) => match result {
                        Ok(Some(info)) => {
                            log::info!("New version available: {}", info.version);
                            self.new_version = Some(info);
                        }
                        Ok(None) => {
                            log::debug!("Running latest version");
                        }
                        Err(e) => {
                            log::warn!("Version check failed: {}", e);
                        }
                    },
                }
                Task::none()
            }

            Message::OpenReleasePage => {
                if let Some(info) = &self.new_version {
                    let _ = open::that(&info.download_url);
                }
                Task::none()
            }
            Message::MusicPlayer(msg) => {
                // Check if we need to pause or resume the machine
                let was_paused = self.music_player.playback_state == PlaybackState::Paused;

                // Intercept Pause - also pause the machine
                if let MusicPlayerMessage::Pause = &msg {
                    let cmd = self
                        .music_player
                        .update(msg, self.connection.clone())
                        .map(Message::MusicPlayer);

                    // Also send PauseMachine command
                    if let Some(host) = &self.host_url {
                        let url = format!("{}/v1/machine:pause", host);
                        let pause_cmd = Task::perform(
                            async move {
                                let client = reqwest::Client::builder()
                                    .timeout(std::time::Duration::from_secs(REST_TIMEOUT_SECS))
                                    .build()
                                    .map_err(|e| format!("Client error: {}", e))?;
                                client
                                    .put(&url)
                                    .send()
                                    .await
                                    .map_err(|e| format!("Pause failed: {}", e))?;
                                Ok("Machine paused".to_string())
                            },
                            Message::MachineCommandCompleted,
                        );
                        return Task::batch([cmd, pause_cmd]);
                    }
                    return cmd;
                }

                // Intercept Play when resuming from pause - also resume the machine
                if let MusicPlayerMessage::Play = &msg {
                    if was_paused {
                        let cmd = self
                            .music_player
                            .update(msg, self.connection.clone())
                            .map(Message::MusicPlayer);

                        // Also send ResumeMachine command
                        if let Some(host) = &self.host_url {
                            let url = format!("{}/v1/machine:resume", host);
                            let resume_cmd = Task::perform(
                                async move {
                                    let client = reqwest::Client::builder()
                                        .timeout(std::time::Duration::from_secs(REST_TIMEOUT_SECS))
                                        .build()
                                        .map_err(|e| format!("Client error: {}", e))?;
                                    client
                                        .put(&url)
                                        .send()
                                        .await
                                        .map_err(|e| format!("Resume failed: {}", e))?;
                                    Ok("Machine resumed".to_string())
                                },
                                Message::MachineCommandCompleted,
                            );
                            return Task::batch([cmd, resume_cmd]);
                        }
                        return cmd;
                    }
                }

                self.music_player
                    .update(msg, self.connection.clone())
                    .map(Message::MusicPlayer)
            }

            Message::ConfigEditor(msg) => self
                .config_editor
                .update(
                    msg,
                    self.connection.clone(),
                    self.host_url.clone(),
                    self.settings.connection.password.clone(),
                )
                .map(Message::ConfigEditor),

            Message::HostInputChanged(value) => {
                self.host_input = value;
                Task::none()
            }

            Message::PasswordInputChanged(value) => {
                self.password_input = value;
                Task::none()
            }

            Message::ConnectPressed => {
                log::info!("Connect button pressed");
                let conn_settings = ConnectionSettings {
                    host: self.host_input.clone(),
                    password: if self.password_input.is_empty() {
                        None
                    } else {
                        Some(self.password_input.clone())
                    },
                    stream_control_method: self.settings.connection.stream_control_method,
                };
                self.profile_manager.active_settings_mut().connection = conn_settings;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::warn!("Could not save profiles: {}", e);
                }
                self.establish_connection();
                // Trigger status refresh and remote browser refresh after a short delay
                Task::perform(
                    async {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    },
                    |_| Message::RefreshAfterConnect,
                )
            }

            Message::DisconnectPressed => {
                log::info!("Disconnecting...");

                // Stop video streaming if active to prevent hangs
                if self.video_streaming.is_streaming {
                    let _ = self
                        .video_streaming
                        .update(StreamingMessage::StopStream, None);
                }

                self.connection = None;
                self.host_url = None;
                self.status.connected = false;
                self.status.device_info = None;
                self.status.mounted_disks.clear();
                self.remote_browser.set_host(None, None);
                // Clear telnet host for video streaming control
                self.video_streaming.set_ultimate_host(None);
                self.user_message = Some(UserMessage::Info(
                    "Disconnected from Ultimate64".to_string(),
                ));
                Task::none()
            }
            Message::StreamControlMethodChanged(method) => {
                self.profile_manager
                    .active_settings_mut()
                    .connection
                    .stream_control_method = method;
                self.settings = self.profile_manager.active_settings().clone();
                self.video_streaming.set_stream_control_method(method);
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                Task::none()
            }
            Message::RefreshStatus => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Task::none()
                }
            }
            Message::ProfileSelected(name) => {
                if self.profile_manager.switch_profile(&name) {
                    self.settings = self.profile_manager.active_settings().clone();
                    self.host_input = self.settings.connection.host.clone();
                    self.password_input = self
                        .settings
                        .connection
                        .password
                        .clone()
                        .unwrap_or_default();
                    self.font_size_input = self.settings.preferences.font_size.to_string();
                    self.video_streaming
                        .set_stream_control_method(self.settings.connection.stream_control_method);
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message =
                        Some(UserMessage::Info(format!("Switched to profile: {}", name)));
                    // Disconnect when switching profiles
                    return Task::done(Message::DisconnectPressed);
                }
                Task::none()
            }

            Message::NewProfileNameChanged(name) => {
                self.new_profile_name = name;
                Task::none()
            }

            Message::CreateProfile => {
                let name = self.new_profile_name.trim().to_string();
                if name.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name cannot be empty".to_string(),
                    ));
                } else if self.profile_manager.add_profile(name.clone()) {
                    self.new_profile_name.clear();
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message =
                        Some(UserMessage::Info(format!("Created profile: {}", name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name already exists".to_string(),
                    ));
                }
                Task::none()
            }

            Message::DuplicateProfile => {
                let new_name = format!("{} (copy)", self.profile_manager.active_profile);
                if self.profile_manager.duplicate_profile(
                    &self.profile_manager.active_profile.clone(),
                    new_name.clone(),
                ) {
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message =
                        Some(UserMessage::Info(format!("Duplicated to: {}", new_name)));
                }
                Task::none()
            }

            Message::DeleteProfile => {
                let name = self.profile_manager.active_profile.clone();
                if self.profile_manager.delete_profile(&name) {
                    self.settings = self.profile_manager.active_settings().clone();
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message =
                        Some(UserMessage::Info(format!("Deleted profile: {}", name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Cannot delete active or last profile".to_string(),
                    ));
                }
                Task::none()
            }

            Message::RenameProfileNameChanged(name) => {
                self.rename_profile_name = name;
                Task::none()
            }

            Message::RenameProfile => {
                let new_name = self.rename_profile_name.trim().to_string();
                let old_name = self.profile_manager.active_profile.clone();
                if new_name.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name cannot be empty".to_string(),
                    ));
                } else if self
                    .profile_manager
                    .rename_profile(&old_name, new_name.clone())
                {
                    self.rename_profile_name.clear();
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message =
                        Some(UserMessage::Info(format!("Renamed to: {}", new_name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name already exists".to_string(),
                    ));
                }
                Task::none()
            }
            Message::CsdbBrowser(msg) => self
                .csdb_browser
                .update(msg, self.connection.clone())
                .map(Message::CsdbBrowser),
            Message::RefreshAfterConnect => {
                // Refresh both status and remote browser after connection
                let status_cmd = if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Task::none()
                };

                let browser_cmd = self
                    .remote_browser
                    .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                    .map(Message::RemoteBrowser);

                Task::batch(vec![status_cmd, browser_cmd])
            }

            Message::StatusUpdated(result) => {
                match result {
                    Ok(status) => {
                        log::debug!(
                            "Status: Connected={}, Device={:?}, Disks={}",
                            status.connected,
                            status.device_info,
                            status.mounted_disks.len()
                        );
                        // Show connected message when first connecting
                        if !self.status.connected && status.connected {
                            self.user_message = Some(UserMessage::Info(format!(
                                "Connected to {}",
                                self.settings.connection.host
                            )));
                        }
                        self.status = status;
                    }
                    Err(e) => {
                        log::error!("Status update failed: {}", e);
                        // Only show error if we were previously connected (lost connection)
                        if self.status.connected {
                            self.user_message =
                                Some(UserMessage::Error(format!("Connection lost: {}", e)));
                            // Stop streaming if connection is lost
                            if self.video_streaming.is_streaming {
                                let _ = self
                                    .video_streaming
                                    .update(StreamingMessage::StopStream, None);
                            }
                        }
                        self.status.connected = false;
                        self.status.device_info = None;
                    }
                }
                Task::none()
            }

            Message::TemplateSelected(template) => {
                self.selected_template = Some(template);
                Task::none()
            }

            Message::ExecuteTemplate => {
                if let Some(template) = &self.selected_template {
                    if let Some(conn) = &self.connection {
                        let conn = conn.clone();
                        let commands = template.commands.clone();
                        return Task::perform(
                            async move { execute_template_commands(conn, commands).await },
                            |result| match result {
                                Ok(_) => Message::RefreshStatus,
                                Err(e) => Message::ShowError(e),
                            },
                        );
                    } else {
                        self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    }
                }
                Task::none()
            }

            Message::ShowError(error) => {
                log::error!("Error: {}", error);
                self.user_message = Some(UserMessage::Error(error));
                Task::none()
            }

            Message::ShowInfo(info) => {
                log::info!("Info: {}", info);
                self.user_message = Some(UserMessage::Info(info));
                Task::none()
            }

            Message::DismissMessage => {
                self.user_message = None;
                Task::none()
            }

            Message::Streaming(msg) => {
                self.video_streaming
                    .set_stream_control_method(self.settings.connection.stream_control_method);

                self.video_streaming
                    .set_api_password(self.settings.connection.password.clone());
                // Handle screenshot result for user message
                if let StreamingMessage::OpenInSeparateWindow = msg {
                    return Task::perform(async {}, |_| Message::OpenStreamingWindow);
                }
                if let StreamingMessage::ScreenshotComplete(ref result) = msg {
                    match result {
                        Ok(path) => {
                            self.user_message =
                                Some(UserMessage::Info(format!("Screenshot saved: {}", path)));
                        }
                        Err(e) => {
                            self.user_message =
                                Some(UserMessage::Error(format!("Screenshot failed: {}", e)));
                        }
                    }
                }

                // Handle fullscreen toggle - change window mode for true fullscreen
                if let StreamingMessage::ToggleFullscreen = msg {
                    self.video_streaming.is_fullscreen = !self.video_streaming.is_fullscreen;

                    let mode = if self.video_streaming.is_fullscreen {
                        iced::window::Mode::Fullscreen
                    } else {
                        iced::window::Mode::Windowed
                    };

                    // Determine which window to make fullscreen
                    if let Some(streaming_id) = self.streaming_window_id {
                        return window::set_mode(streaming_id, mode)
                            .map(|_: ()| Message::RefreshStatus);
                    } else {
                        return iced::window::oldest()
                            .and_then(move |id| iced::window::set_mode(id, mode))
                            .map(|_: ()| Message::RefreshStatus);
                    }
                }

                // Handle open in new window request
                if let StreamingMessage::OpenInSeparateWindow = msg {
                    return Task::done(Message::OpenStreamingWindow);
                }

                // Handle keyboard command - intercept before passing to streaming
                if let StreamingMessage::SendCommand = msg {
                    let command = self.video_streaming.command_input.clone();
                    if !command.is_empty() {
                        if let Some(conn) = &self.connection {
                            let conn = conn.clone();
                            let cmd = command.clone();
                            // Add to history with prompt
                            self.video_streaming
                                .command_history
                                .push(format!("> {}", command));
                            self.video_streaming.command_input.clear();

                            return Task::perform(
                                async move {
                                    let result = tokio::time::timeout(
                                        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                        tokio::task::spawn_blocking(move || {
                                            let conn = conn.blocking_lock();
                                            // Add newline to execute the command
                                            conn.type_text(&format!("{}\n", cmd))
                                                .map(|_| format!("Sent: {}", cmd))
                                                .map_err(|e| format!("Failed to send: {}", e))
                                        }),
                                    )
                                    .await;

                                    match result {
                                        Ok(Ok(r)) => r,
                                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                        Err(_) => {
                                            Err("Command timed out. The device may be offline or Web Remote Control is disabled."
                                                .to_string())
                                        }
                                    }
                                },
                                |result| match result {
                                    Ok(msg) => {
                                        Message::Streaming(StreamingMessage::CommandSent(Ok(msg)))
                                    }
                                    Err(e) => {
                                        Message::Streaming(StreamingMessage::CommandSent(Err(e)))
                                    }
                                },
                            );
                        } else {
                            self.video_streaming
                                .command_history
                                .push("> ".to_string() + &command);
                            self.video_streaming
                                .command_history
                                .push("Error: Not connected to Ultimate64".to_string());
                            self.video_streaming.command_input.clear();
                        }
                    }
                    return Task::none();
                }

                self.video_streaming
                    .update(msg, self.connection.clone())
                    .map(Message::Streaming)
            }

            Message::ExitFullscreen => {
                // Only exit fullscreen if currently in fullscreen mode
                if self.video_streaming.is_fullscreen {
                    self.video_streaming.is_fullscreen = false;
                    // Exit fullscreen on the appropriate window
                    if let Some(streaming_id) = self.streaming_window_id {
                        return window::set_mode(streaming_id, iced::window::Mode::Windowed)
                            .map(|_: ()| Message::RefreshStatus);
                    } else {
                        return iced::window::oldest()
                            .and_then(|id| iced::window::set_mode(id, iced::window::Mode::Windowed))
                            .map(|_: ()| Message::RefreshStatus);
                    }
                }
                Task::none()
            }

            Message::OpenStreamingWindow => {
                if self.streaming_window_id.is_some() {
                    // Window already open
                    return Task::none();
                }
                let settings = iced::window::Settings {
                    size: iced::Size::new(800.0, 600.0),
                    min_size: Some(iced::Size::new(400.0, 300.0)),
                    decorations: true,
                    ..Default::default()
                };
                // Destructure the tuple - open() returns (Id, Task<Id>)
                let (id, open_task) = iced::window::open(settings);
                self.streaming_window_id = Some(id);
                open_task.map(move |_| Message::StreamingWindowOpened(id))
            }

            Message::StreamingWindowOpened(id) => {
                log::info!("Streaming window opened: {:?}", id);
                // ID already stored in OpenStreamingWindow handler
                Task::none()
            }

            Message::WindowClosed(id) => {
                if self.streaming_window_id == Some(id) {
                    // Streaming window was closed
                    log::info!("Streaming window closed: {:?}", id);
                    self.streaming_window_id = None;
                    Task::none()
                } else if self.main_window_id == Some(id) {
                    // Main window was closed - exit the application
                    log::info!("Main window closed: {:?}", id);
                    // Stop streaming if active
                    if self.video_streaming.is_streaming {
                        self.video_streaming
                            .stop_signal
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    // Close any remaining windows and exit
                    if let Some(streaming_id) = self.streaming_window_id {
                        return iced::window::close(streaming_id);
                    }
                    iced::exit()
                } else {
                    Task::none()
                }
            }
            Message::ResetMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.reset()
                                        .map(|_| "Machine reset successfully".to_string())
                                        .map_err(|e| format!("Reset failed: {}", e))
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => {
                                    Err("Reset timed out - device may be offline".to_string())
                                }
                            }
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::RebootMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:reboot", host);
                    Task::perform(
                        async move {
                            let client = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(REST_TIMEOUT_SECS))
                                .build()
                                .map_err(|e| format!("Client error: {}", e))?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Reboot failed: {}", e))?;
                            Ok("Machine rebooting...".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::PauseMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:pause", host);
                    Task::perform(
                        async move {
                            let client = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(REST_TIMEOUT_SECS))
                                .build()
                                .map_err(|e| format!("Client error: {}", e))?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Pause failed: {}", e))?;
                            Ok("Machine paused".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::ResumeMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:resume", host);
                    Task::perform(
                        async move {
                            let client = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(REST_TIMEOUT_SECS))
                                .build()
                                .map_err(|e| format!("Client error: {}", e))?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Resume failed: {}", e))?;
                            Ok("Machine resumed".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::PoweroffMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.poweroff()
                                        .map(|_| "Machine powered off".to_string())
                                        .map_err(|e| format!("Poweroff failed: {}", e))
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => {
                                    Err("Poweroff timed out - device may be offline".to_string())
                                }
                            }
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::MachineCommandCompleted(result) => {
                match result {
                    Ok(msg) => {
                        self.user_message = Some(UserMessage::Info(msg));
                    }
                    Err(e) => {
                        self.user_message = Some(UserMessage::Error(e));
                    }
                }
                Task::none()
            }
            Message::DefaultSongDurationChanged(value) => {
                if let Ok(duration) = value.parse::<u32>() {
                    if duration > 0 && duration <= 3600 {
                        self.profile_manager
                            .active_settings_mut()
                            .preferences
                            .default_song_duration = duration;
                        self.settings = self.profile_manager.active_settings().clone();
                        self.music_player.set_default_song_duration(duration);
                        if let Err(e) = self.profile_manager.save() {
                            log::error!("Failed to save profiles: {}", e);
                        }
                    }
                }
                Task::none()
            }

            Message::FontSizeChanged(value) => {
                self.font_size_input = value.clone();
                if let Ok(size) = value.parse::<u32>() {
                    if size >= 8 && size <= 24 {
                        self.profile_manager
                            .active_settings_mut()
                            .preferences
                            .font_size = size;
                        self.settings = self.profile_manager.active_settings().clone();
                        if let Err(e) = self.profile_manager.save() {
                            log::error!("Failed to save profiles: {}", e);
                        }
                    }
                }
                Task::none()
            }
            // Starting directory settings
            Message::BrowseFileBrowserStartDir => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::FileBrowserStartDirSelected,
            ),

            Message::FileBrowserStartDirSelected(path) => {
                if let Some(p) = path {
                    self.profile_manager
                        .active_settings_mut()
                        .default_paths
                        .file_browser_start_dir = Some(p);
                    self.settings = self.profile_manager.active_settings().clone();
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message = Some(UserMessage::Info(
                        "File Browser start directory set (restart app to apply)".to_string(),
                    ));
                }
                Task::none()
            }

            Message::ClearFileBrowserStartDir => {
                self.profile_manager
                    .active_settings_mut()
                    .default_paths
                    .file_browser_start_dir = None;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "File Browser start directory cleared".to_string(),
                ));
                Task::none()
            }

            Message::BrowseMusicPlayerStartDir => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::MusicPlayerStartDirSelected,
            ),

            Message::MusicPlayerStartDirSelected(path) => {
                if let Some(p) = path {
                    self.profile_manager
                        .active_settings_mut()
                        .default_paths
                        .music_player_start_dir = Some(p);
                    self.settings = self.profile_manager.active_settings().clone();
                    if let Err(e) = self.profile_manager.save() {
                        log::error!("Failed to save profiles: {}", e);
                    }
                    self.user_message = Some(UserMessage::Info(
                        "Music Player start directory set (restart app to apply)".to_string(),
                    ));
                }
                Task::none()
            }

            Message::ClearMusicPlayerStartDir => {
                self.profile_manager
                    .active_settings_mut()
                    .default_paths
                    .music_player_start_dir = None;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "Music Player start directory cleared".to_string(),
                ));
                Task::none()
            }
        }
    }

    fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        // Check if this is the streaming window
        if Some(window_id) == self.streaming_window_id {
            return self
                .video_streaming
                .view_separate_window()
                .map(Message::Streaming);
        }

        // If video is in fullscreen mode in main window, show only the fullscreen view
        if self.video_streaming.is_fullscreen {
            return self
                .video_streaming
                .view_fullscreen()
                .map(Message::Streaming);
        }

        // Tab bar with retro style
        let tabs = container(
            row![
                self.tab_button("FILE BROWSER", Tab::DualPaneBrowser),
                self.tab_button("MUSIC PLAYER", Tab::MusicPlayer),
                self.tab_button("VIDEO VIEWER", Tab::VideoViewer),
                self.tab_button("MEMORY", Tab::MemoryEditor),
                self.tab_button("CONFIG", Tab::Configuration),
                self.tab_button("CSDB", Tab::CsdbBrowser),
                self.tab_button("SETTINGS", Tab::Settings),
            ]
            .spacing(2),
        )
        .padding(5);

        // Connection status bar at top
        let connection_bar = self.view_connection_bar();

        // Main content area
        let content = container(match self.active_tab {
            Tab::DualPaneBrowser => self.view_dual_pane_browser(),
            Tab::MusicPlayer => self.view_music_player(),
            Tab::VideoViewer => {
                if self.streaming_window_id.is_some() {
                    // Streaming is shown in separate window - show placeholder
                    container(
                        column![
                            text("Video is displayed in separate window").size(16),
                            Space::new(),
                            button(text("Close Separate Window").size(12))
                                .on_press(Message::CloseStreamingWindow)
                                .padding([8, 16]),
                        ]
                        .align_x(iced::Alignment::Center)
                        .spacing(10),
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
                } else {
                    self.video_streaming.view().map(Message::Streaming)
                }
            }
            Tab::MemoryEditor => self
                .memory_editor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::MemoryEditor),
            Tab::Configuration => self
                .config_editor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::ConfigEditor),
            Tab::CsdbBrowser => self
                .csdb_browser
                .view(self.settings.preferences.font_size, self.status.connected)
                .map(Message::CsdbBrowser),
            Tab::Settings => self.view_settings(),
        })
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill);

        // Bottom status/control bar
        let status_bar = self.view_status_bar();

        let main_content = column![
            connection_bar,
            rule::horizontal(1),
            tabs,
            rule::horizontal(1),
            content,
            rule::horizontal(1),
            status_bar
        ]
        .spacing(0);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        use iced::event::{self, Event};
        use iced::keyboard::{self, Key};
        use std::time::Duration;

        // Keyboard shortcuts: ESC to exit fullscreen, Opt+F (macOS) or Alt+F (Windows/Linux) to toggle
        let keyboard_sub = event::listen_with(|event, _status, _id| {
            if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
                match key {
                    Key::Named(keyboard::key::Named::Escape) => Some(Message::ExitFullscreen),
                    Key::Character(ref c) if c.as_str() == "f" && modifiers.alt() => Some(
                        Message::Streaming(streaming::StreamingMessage::ToggleFullscreen),
                    ),
                    _ => None,
                }
            } else {
                None
            }
        });

        // Window close event listener for streaming window
        let window_events = iced::event::listen_with(|event, _status, id| {
            if let iced::Event::Window(iced::window::Event::Closed) = event {
                Some(Message::WindowClosed(id))
            } else {
                None
            }
        });

        // Periodic connection check every 60 seconds (only when connected)
        let status_check = if self.status.connected {
            iced::time::every(Duration::from_secs(60)).map(|_| Message::RefreshStatus)
        } else {
            Subscription::none()
        };

        Subscription::batch([
            self.video_streaming.subscription().map(Message::Streaming),
            self.music_player.subscription().map(Message::MusicPlayer),
            keyboard_sub,
            window_events,
            status_check,
        ])
    }

    fn tab_button<'a>(&self, label: &'a str, tab: Tab) -> Element<'a, Message> {
        let is_active = self.active_tab == tab;
        button(text(label).size(14))
            .on_press(Message::TabSelected(tab))
            .padding([8, 16])
            .style(if is_active {
                button::primary
            } else {
                button::secondary
            })
            .into()
    }

    fn view_connection_bar(&self) -> Element<'_, Message> {
        let status_indicator = if self.status.connected {
            text("● CONNECTED").color(iced::Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            text("○ DISCONNECTED").color(iced::Color::from_rgb(0.8, 0.2, 0.2))
        };

        let device_text = text(self.status.device_info.as_deref().unwrap_or("No device")).size(12);

        // Update notification on the right side
        let update_notification: Element<'_, Message> = if let Some(info) = &self.new_version {
            row![
                text(format!("🎉 {} available!", info.version))
                    .size(12)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                button(text("Download").size(11))
                    .on_press(Message::OpenReleasePage)
                    .padding([2, 8])
                    .style(button::primary),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            Space::new().into()
        };

        container(
            row![
                status_indicator,
                text(" | ").size(12),
                device_text,
                Space::new().width(Length::Fill),
                update_notification,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
    }

    fn view_dual_pane_browser(&self) -> Element<'_, Message> {
        // Left pane - Local files
        let left_header = row![
            text("LOCAL").size(12).width(Length::Fill),
            if self.active_pane == Pane::Left {
                text("[ACTIVE]").size(10)
            } else {
                text("").size(10)
            }
        ]
        .padding(5)
        .align_y(iced::Alignment::Center);

        let left_pane = container(column![
            left_header,
            rule::horizontal(1),
            self.left_browser
                .view(self.settings.preferences.font_size)
                .map(Message::LeftBrowser),
        ])
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(if self.active_pane == Pane::Left { 2 } else { 0 });

        // Copy buttons in center
        let copy_buttons = container(
            column![
                tooltip(
                    button(text(">>").size(14))
                        .on_press(Message::CopyLocalToRemote)
                        .padding([8, 12]),
                    "Upload checked files to Ultimate64",
                    tooltip::Position::Right,
                )
                .style(container::bordered_box),
                Space::new().height(10),
                tooltip(
                    button(text("<<").size(14))
                        .on_press(Message::CopyRemoteToLocal)
                        .padding([8, 12]),
                    "Download selected file from Ultimate64",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            ]
            .align_x(iced::Alignment::Center),
        )
        .padding([20, 5])
        .center_y(Length::Shrink);

        // Right pane - Ultimate64 files
        let connection_indicator = if self.status.connected { "*" } else { "" };
        let right_header = row![
            text(format!("ULTIMATE64 {}", connection_indicator))
                .size(12)
                .width(Length::Fill),
            tooltip(
                button(text("Refresh").size(10))
                    .on_press(Message::RemoteBrowser(RemoteBrowserMessage::RefreshFiles))
                    .padding([2, 6]),
                "Refresh remote file listing",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .padding(5)
        .align_y(iced::Alignment::Center);

        let right_pane = container(column![
            right_header,
            rule::horizontal(1),
            self.remote_browser
                .view(self.settings.preferences.font_size)
                .map(Message::RemoteBrowser),
        ])
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(if self.active_pane == Pane::Right {
            2
        } else {
            0
        });

        // Template section at bottom
        let template_section = container(
            row![
                text("Quick Actions:").size(12),
                pick_list(
                    self.template_manager.get_templates(),
                    self.selected_template.clone(),
                    Message::TemplateSelected,
                )
                .placeholder("Select template...")
                .width(Length::Fixed(200.0)),
                tooltip(
                    button(text("Execute").size(12))
                        .on_press(Message::ExecuteTemplate)
                        .padding([4, 12]),
                    "Run the selected template commands",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding(10);

        column![
            row![left_pane, copy_buttons, right_pane].height(Length::Fill),
            rule::horizontal(1),
            template_section,
        ]
        .into()
    }

    fn view_music_player(&self) -> Element<'_, Message> {
        self.music_player
            .view(self.settings.preferences.font_size)
            .map(Message::MusicPlayer)
    }

    fn view_settings(&self) -> Element<'_, Message> {
        // Profile management section
        let profile_names = self.profile_manager.profile_names();
        let profile_section = column![
            text("CONFIGURATION PROFILES").size(18),
            Space::new().height(10),
            row![
                text("Active Profile:").size(14),
                pick_list(
                    profile_names,
                    Some(self.profile_manager.active_profile.clone()),
                    Message::ProfileSelected,
                )
                .width(Length::Fixed(200.0)),
                tooltip(
                    button(text("Duplicate").size(11))
                        .on_press(Message::DuplicateProfile)
                        .padding([4, 10]),
                    "Create a copy of current profile",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Delete").size(11))
                        .on_press(Message::DeleteProfile)
                        .padding([4, 10]),
                    "Delete current profile (cannot delete last profile)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            Space::new().height(10),
            row![
                text("New Profile:").size(14),
                text_input("Profile name...", &self.new_profile_name)
                    .on_input(Message::NewProfileNameChanged)
                    .on_submit(Message::CreateProfile)
                    .padding(8)
                    .width(Length::Fixed(200.0)),
                button(text("Create").size(11))
                    .on_press(Message::CreateProfile)
                    .padding([4, 10]),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            text("Use profiles to store different configurations for different machines").size(11),
        ];
        // Discovery button
        // Discovery button
        let discovery_button: Element<'_, Message> = if self.is_discovering {
            button(text("Scanning...").size(11)).padding([4, 10]).into()
        } else {
            tooltip(
                button(text("🔍 Find Devices").size(11))
                    .on_press(Message::StartDiscovery)
                    .padding([4, 10]),
                "Scan local network for Ultimate64 devices",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box)
            .into()
        };

        // List of discovered devices
        // List of discovered devices
        let discovered_list: Element<'_, Message> = if self.discovered_devices.is_empty() {
            if self.is_discovering {
                text("Scanning network...").size(11).into()
            } else {
                text("").size(1).into()
            }
        } else {
            column(
                self.discovered_devices
                    .iter()
                    .map(|d| {
                        let device = d.clone();
                        let label = format!("{} - {} ({})", d.ip, d.product, d.firmware);
                        button(text(label).size(11))
                            .on_press(Message::SelectDiscoveredDevice(device))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(button::secondary)
                            .into()
                    })
                    .collect::<Vec<_>>(),
            )
            .spacing(2)
            .width(Length::Fixed(400.0))
            .into()
        };
        let connection_section = column![
            text("CONNECTION SETTINGS").size(18),
            Space::new().height(10),
            row![text("Ultimate64 IP Address:").size(14), discovery_button,]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            text_input("192.168.1.64", &self.host_input)
                .on_input(Message::HostInputChanged)
                .padding(10)
                .width(Length::Fixed(300.0)),
            discovered_list,
            Space::new().height(10),
            text("Password (optional):").size(14),
            text_input("Enter password...", &self.password_input)
                .on_input(Message::PasswordInputChanged)
                .padding(10)
                .width(Length::Fixed(300.0)),
            Space::new().height(10),
            text("Stream Control Method:").size(14),
            row![
                pick_list(
                    &StreamControlMethod::ALL[..],
                    Some(self.settings.connection.stream_control_method),
                    Message::StreamControlMethodChanged,
                )
                .width(Length::Fixed(250.0)),
            ]
            .spacing(10),
            text("Controls how video/audio streaming communicates with the Ultimate64").size(11),
            Space::new().height(15),
            row![
                tooltip(
                    button(text("Connect"))
                        .on_press(Message::ConnectPressed)
                        .padding([10, 20]),
                    "Connect to Ultimate64 and save settings",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Disconnect"))
                        .on_press(Message::DisconnectPressed)
                        .padding([10, 20]),
                    "Disconnect from Ultimate64",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Test Connection"))
                        .on_press(Message::RefreshStatus)
                        .padding([10, 20]),
                    "Test connection and refresh status",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(10),
        ];
        let status_section = column![
            Space::new().height(20),
            rule::horizontal(1),
            Space::new().height(10),
            text("CONNECTION STATUS").size(18),
            Space::new().height(10),
            if self.status.connected {
                text(format!("Connected to {}", self.settings.connection.host))
                    .color(iced::Color::from_rgb(0.2, 0.8, 0.2))
            } else {
                text("Not connected").color(iced::Color::from_rgb(0.8, 0.2, 0.2))
            },
            if let Some(info) = &self.status.device_info {
                text(format!("Device: {}", info)).size(14)
            } else {
                text("").size(14)
            },
        ];

        // Starting directories section
        let file_browser_start_dir_display = self
            .settings
            .default_paths
            .file_browser_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());

        let music_player_start_dir_display = self
            .settings
            .default_paths
            .music_player_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());

        let starting_dirs_section = column![
            Space::new().height(20),
            rule::horizontal(1),
            Space::new().height(10),
            text("STARTING DIRECTORIES").size(18),
            Space::new().height(10),
            text("File Browser tab starting directory:").size(14),
            row![
                text(file_browser_start_dir_display.clone())
                    .size(12)
                    .width(Length::Fixed(400.0)),
                tooltip(
                    button(text("Browse").size(11))
                        .on_press(Message::BrowseFileBrowserStartDir)
                        .padding([4, 10]),
                    "Select starting directory for File Browser",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Clear").size(11))
                        .on_press(Message::ClearFileBrowserStartDir)
                        .padding([4, 10]),
                    "Reset to home directory",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            Space::new().height(10),
            text("Music Player tab starting directory:").size(14),
            row![
                text(music_player_start_dir_display.clone())
                    .size(12)
                    .width(Length::Fixed(400.0)),
                tooltip(
                    button(text("Browse").size(11))
                        .on_press(Message::BrowseMusicPlayerStartDir)
                        .padding([4, 10]),
                    "Select starting directory for Music Player",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Clear").size(11))
                        .on_press(Message::ClearMusicPlayerStartDir)
                        .padding([4, 10]),
                    "Reset to home directory",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
            text("(Changes take effect on next application restart)").size(11),
        ];

        let music_section = column![
            Space::new().height(20),
            rule::horizontal(1),
            Space::new().height(10),
            text("MUSIC PLAYER SETTINGS").size(18),
            Space::new().height(10),
            row![
                text("Default song duration (seconds):").size(14),
                text_input(
                    "180",
                    &self.settings.preferences.default_song_duration.to_string()
                )
                .on_input(Message::DefaultSongDurationChanged)
                .padding(8)
                .width(Length::Fixed(80.0)),
                text("(used when song length is unknown)").size(11),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        ];

        let ui_section = column![
            Space::new().height(20),
            rule::horizontal(1),
            Space::new().height(10),
            text("UI SETTINGS").size(18),
            Space::new().height(10),
            row![
                text("Font size:").size(14),
                text_input("12", &self.font_size_input)
                    .on_input(Message::FontSizeChanged)
                    .padding(8)
                    .width(Length::Fixed(60.0)),
                text("(8-24, applies to File Browser and Music Player)").size(11),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        ];

        let debug_section = column![
            Space::new().height(20),
            rule::horizontal(1),
            Space::new().height(10),
            text("DEBUG INFO").size(18),
            text(format!("Platform: {}", std::env::consts::OS)).size(12),
            text(format!("Config dir: {:?}", dirs::config_dir())).size(12),
            text(format!(
                "Active profile: {}",
                self.profile_manager.active_profile
            ))
            .size(12),
            text(format!(
                "Total profiles: {}",
                self.profile_manager.profiles.len()
            ))
            .size(12),
        ];

        scrollable(container(
            column![
                profile_section,
                Space::new().height(20),
                rule::horizontal(1),
                Space::new().height(10),
                connection_section,
                status_section,
                starting_dirs_section,
                music_section,
                ui_section,
                debug_section
            ]
            .spacing(5)
            .padding(20),
        ))
        .height(Length::Fill)
        .into()
    }

    fn view_status_bar(&self) -> Element<'_, Message> {
        let video_status = if self.video_streaming.is_streaming {
            "STREAMING"
        } else {
            "IDLE"
        };

        // Show user message if present, otherwise show video status
        let status_text: Element<'_, Message> = if let Some(msg) = &self.user_message {
            let (prefix, message, is_error) = match msg {
                UserMessage::Error(e) => ("ERROR: ", e.as_str(), true),
                UserMessage::Info(i) => ("", i.as_str(), false),
            };
            let color = if is_error {
                iced::Color::from_rgb(0.8, 0.0, 0.0)
            } else {
                iced::Color::from_rgb(0.0, 0.5, 0.0)
            };

            // Check if this is a screenshot message - make path clickable
            if message.starts_with("Screenshot saved: ") {
                let path = message
                    .strip_prefix("Screenshot saved: ")
                    .unwrap_or(message);
                row![
                    text("Screenshot saved: ").size(12).color(color),
                    button(
                        text(path)
                            .size(12)
                            .color(iced::Color::from_rgb(0.3, 0.6, 1.0))
                    )
                    .style(button::text)
                    .on_press(Message::Streaming(StreamingMessage::OpenScreenshot(
                        path.to_string()
                    )))
                    .padding(0),
                    tooltip(
                        button(text("X").size(10))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                row![
                    text(format!("{}{}", prefix, message)).size(12).color(color),
                    tooltip(
                        button(text("X").size(10))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .into()
            }
        } else {
            text(video_status).size(12).into()
        };

        container(
            row![
                status_text,
                Space::new().width(Length::Fill),
                tooltip(
                    button(text("PAUSE").size(11))
                        .on_press(Message::PauseMachine)
                        .padding([4, 8]),
                    "Pause the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("RESUME").size(11))
                        .on_press(Message::ResumeMachine)
                        .padding([4, 8]),
                    "Resume the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text("|").size(12),
                tooltip(
                    button(text("RESET").size(11))
                        .on_press(Message::ResetMachine)
                        .padding([4, 8]),
                    "Reset the C64 (soft reset)",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("REBOOT").size(11))
                        .on_press(Message::RebootMachine)
                        .padding([4, 8]),
                    "Reboot the Ultimate64 device",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("POWER OFF").size(11))
                        .on_press(Message::PoweroffMachine)
                        .padding([4, 8]),
                    "Power off the Ultimate64",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
    }

    fn establish_connection(&mut self) {
        self.connection = None;
        self.host_url = None;
        self.status.connected = false;
        self.status.device_info = None;
        self.status.mounted_disks.clear();

        if self.settings.connection.host.is_empty() {
            log::error!("Host IP is empty");
            self.user_message = Some(UserMessage::Error("Host IP cannot be empty".to_string()));
            return;
        }

        log::info!(
            "Attempting to connect to: {}",
            self.settings.connection.host
        );

        // Parse host
        let host = if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv4Addr>()
        {
            log::debug!("Parsed as IPv4: {}", ip_addr);
            Host::Ipv4(ip_addr)
        } else if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv6Addr>() {
            log::debug!("Parsed as IPv6: {}", ip_addr);
            Host::Ipv6(ip_addr)
        } else {
            match Host::parse(&self.settings.connection.host) {
                Ok(h) => {
                    log::debug!("Parsed as domain: {:?}", h);
                    h
                }
                Err(e) => {
                    log::error!(
                        "Failed to parse host '{}': {}",
                        self.settings.connection.host,
                        e
                    );
                    self.user_message = Some(UserMessage::Error(format!("Invalid host: {}", e)));
                    return;
                }
            }
        };

        log::info!("Creating REST connection...");
        match Rest::new(&host, self.settings.connection.password.clone()) {
            Ok(rest) => {
                log::info!("REST client created, verifying connection...");
                self.connection = Some(Arc::new(TokioMutex::new(rest)));
                // Build proper HTTP URL
                let http_url = format!("http://{}", self.settings.connection.host);
                self.host_url = Some(http_url.clone());
                // Don't set connected = true here - let StatusUpdated verify the connection
                self.remote_browser
                    .set_host(Some(http_url), self.settings.connection.password.clone());
                // Set telnet host for video streaming control
                self.video_streaming
                    .set_ultimate_host(Some(self.settings.connection.host.clone()));
                self.user_message = Some(UserMessage::Info(format!(
                    "Connecting to {}...",
                    self.settings.connection.host
                )));
            }
            Err(e) => {
                log::error!("Connection failed: {}", e);
                self.user_message = Some(UserMessage::Error(format!("Connection failed: {}", e)));
            }
        }
    }
}

async fn execute_template_commands(
    connection: Arc<TokioMutex<Rest>>,
    commands: Vec<String>,
) -> Result<(), String> {
    for command in commands {
        log::info!("Executing: {}", command);
        let conn = connection.clone();

        if command.starts_with("RESET") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.blocking_lock();
                    conn.reset().map_err(|e| format!("Reset failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Reset timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        } else if let Some(text) = command.strip_prefix("TYPE ") {
            let text = text.to_string();
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.blocking_lock();
                    conn.type_text(&text)
                        .map_err(|e| format!("Type failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Type command timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("LOAD") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.blocking_lock();
                    conn.type_text("load\"*\",8,1\n")
                        .map_err(|e| format!("Load failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Load command timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("RUN") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.blocking_lock();
                    conn.type_text("run\n")
                        .map_err(|e| format!("Run failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Run command timed out - device may be offline".to_string()),
            }
        }
    }

    Ok(())
}

/// Timeout for REST API operations to prevent hangs when device goes offline
const REST_TIMEOUT_SECS: u64 = 5;

async fn fetch_status(connection: Arc<TokioMutex<Rest>>) -> Result<StatusInfo, String> {
    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();

            let device_info = match conn.info() {
                Ok(info) => Some(format!("{} ({})", info.product, info.firmware_version)),
                Err(e) => return Err(format!("Failed to get device info: {}", e)),
            };

            let mounted_disks = match conn.drive_list() {
                Ok(drives) => drives
                    .into_iter()
                    .filter_map(|(name, drive)| drive.image_file.map(|file| (name, file)))
                    .collect(),
                Err(_) => Vec::new(),
            };

            Ok(StatusInfo {
                connected: true,
                device_info,
                mounted_disks,
            })
        }),
    )
    .await;

    match result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Connection timed out - device may be offline".to_string()),
    }
}

impl Drop for Ultimate64Browser {
    fn drop(&mut self) {
        log::info!("Application dropping, cleaning up...");

        self.video_streaming
            .stop_signal
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Reset C64 if music was playing (with timeout to prevent hang)
        if self.music_player.playback_state == PlaybackState::Playing {
            if let Some(conn) = &self.connection {
                log::info!("Resetting C64 on drop...");
                let conn = conn.clone();

                let (tx, rx) = std::sync::mpsc::channel();

                std::thread::spawn(move || {
                    let result = if let Ok(c) = conn.try_lock() {
                        c.reset().map_err(|e| e.to_string())
                    } else {
                        Err("Could not acquire lock".to_string())
                    };
                    let _ = tx.send(result);
                });

                match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(Ok(())) => log::info!("C64 reset successful"),
                    Ok(Err(e)) => log::warn!("C64 reset failed: {}", e),
                    Err(_) => log::warn!("C64 reset timed out - device may be offline"),
                }
            }
        }
    }
}
