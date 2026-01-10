// Remove the windows_subsystem attribute during development to see console output
// Uncomment for release builds:
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use iced::{
    Application, Command, Element, Length, Settings, Subscription, Theme, executor,
    widget::{
        Space, button, column, container, horizontal_rule, horizontal_space, pick_list, row,
        scrollable, text, text_input, tooltip,
    },
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;
use url::Host;

mod api;
mod config_editor;
mod file_browser;
mod music_player;
mod remote_browser;
mod settings;
mod streaming;
mod templates;

use config_editor::{ConfigEditor, ConfigEditorMessage};
use file_browser::{FileBrowser, FileBrowserMessage};
use music_player::{MusicPlayer, MusicPlayerMessage, PlaybackState};
use remote_browser::{RemoteBrowser, RemoteBrowserMessage};
use settings::{AppSettings, ConnectionSettings};
use streaming::{StreamingMessage, VideoStreaming};
use templates::{DiskTemplate, TemplateManager};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn main() -> iced::Result {
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

    // Load window icon
    let icon = load_window_icon();

    Ultimate64Browser::run(Settings {
        window: iced::window::Settings {
            size: iced::Size::new(1200.0, 800.0),
            min_size: Some(iced::Size::new(800.0, 600.0)),
            icon: icon,
            ..Default::default()
        },
        ..Default::default()
    })
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    DualPaneBrowser,
    MusicPlayer,
    VideoViewer,
    Configuration,
    Settings,
}

impl std::fmt::Display for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tab::DualPaneBrowser => write!(f, "File Browser"),
            Tab::MusicPlayer => write!(f, "Music Player"),
            Tab::VideoViewer => write!(f, "Video Viewer"),
            Tab::Configuration => write!(f, "Configuration"),
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
}

impl Application for Ultimate64Browser {
    type Message = Message;
    type Theme = Theme;
    type Executor = executor::Default;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        log::info!("Initializing application...");

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

        // Create music player with configured starting directory
        let mut music_player =
            MusicPlayer::new(settings.default_paths.music_player_start_dir.clone());
        music_player.set_default_song_duration(settings.preferences.default_song_duration);

        // Create file browser with configured starting directory
        let left_browser = FileBrowser::new(settings.default_paths.file_browser_start_dir.clone());

        let mut app = Self {
            active_tab: Tab::DualPaneBrowser,
            left_browser,
            remote_browser: RemoteBrowser::new(),
            active_pane: Pane::Left,
            music_player,
            config_editor: ConfigEditor::new(),
            host_input: settings.connection.host.clone(),
            password_input: settings.connection.password.clone().unwrap_or_default(),
            font_size_input: settings.preferences.font_size.to_string(),
            settings: settings.clone(),
            host_url: None,
            template_manager: TemplateManager::new(),
            selected_template: None,
            connection: None,
            status: StatusInfo {
                connected: false,
                device_info: None,
                mounted_disks: Vec::new(),
            },
            user_message: None,
            video_streaming: VideoStreaming::new(),
        };

        // Auto-connect if host is configured
        if !settings.connection.host.is_empty() {
            log::info!(
                "Auto-connecting to configured host: {}",
                settings.connection.host
            );
            app.establish_connection();
            return (
                app,
                Command::perform(
                    async {
                        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                    },
                    |_| Message::RefreshStatus,
                ),
            );
        }

        log::info!("No host configured, waiting for user input");
        (app, Command::none())
    }

    fn title(&self) -> String {
        let connection_status = if self.status.connected {
            format!(" - Connected to {}", self.settings.connection.host)
        } else {
            " - Disconnected".to_string()
        };
        format!("Ultimate64 Manager v{}{}", APP_VERSION, connection_status)
    }

    fn theme(&self) -> Theme {
        // Custom dark theme with lighter blue (like the reference screenshot)
        Theme::custom(
            "Ultimate64 Dark".to_string(),
            iced::theme::Palette {
                background: iced::Color::from_rgb(0.15, 0.15, 0.18), // Dark background
                text: iced::Color::from_rgb(0.9, 0.9, 0.9),          // Light text
                primary: iced::Color::from_rgb(0.45, 0.52, 0.85),    // Lighter blue
                success: iced::Color::from_rgb(0.3, 0.7, 0.3),       // Green
                danger: iced::Color::from_rgb(0.8, 0.3, 0.3),        // Red
            },
        )
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::TabSelected(tab) => {
                log::debug!("Tab selected: {:?}", tab);
                self.active_tab = tab;
                Command::none()
            }

            Message::LeftBrowser(msg) => {
                let should_refresh = matches!(msg, FileBrowserMessage::MountCompleted(Ok(_)));
                let cmd = self
                    .left_browser
                    .update(msg, self.connection.clone())
                    .map(Message::LeftBrowser);

                if should_refresh {
                    Command::batch(vec![
                        cmd,
                        Command::perform(
                            async {
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            },
                            |_| Message::RefreshStatus,
                        ),
                    ])
                } else {
                    cmd
                }
            }

            Message::RemoteBrowser(msg) => {
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
                self.remote_browser
                    .update(msg, self.connection.clone())
                    .map(Message::RemoteBrowser)
            }

            Message::ActivePaneChanged(pane) => {
                self.active_pane = pane;
                Command::none()
            }

            Message::CopyLocalToRemote => {
                // Copy checked local files and directories to Ultimate64
                let items_to_copy = self.left_browser.get_checked_files();

                if items_to_copy.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "No files selected. Use checkboxes to select files.".to_string(),
                    ));
                    return Command::none();
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
                    return Command::none();
                }

                let remote_dest = self.remote_browser.get_current_path().to_string();

                // Build commands for each operation
                let mut commands: Vec<Command<Message>> = Vec::new();

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

                        commands.push(Command::perform(
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
                        return Command::none();
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
                    return Command::none();
                }

                return Command::batch(commands);
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

                        return Command::perform(
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
                Command::none()
            }

            Message::CopyComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.user_message = Some(UserMessage::Info(msg));
                        // Clear checked files after successful copy
                        self.left_browser.clear_checked();
                        // Refresh both browsers
                        return Command::batch(vec![
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
                Command::none()
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
                        let pause_cmd = Command::perform(
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
                        return Command::batch([cmd, pause_cmd]);
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
                            let resume_cmd = Command::perform(
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
                            return Command::batch([cmd, resume_cmd]);
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
                Command::none()
            }

            Message::PasswordInputChanged(value) => {
                self.password_input = value;
                Command::none()
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
                };
                self.settings.connection = conn_settings;
                if let Err(e) = self.settings.save() {
                    log::warn!("Could not save settings: {}", e);
                }
                self.establish_connection();
                // Trigger status refresh and remote browser refresh after a short delay
                Command::perform(
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
                self.user_message = Some(UserMessage::Info(
                    "Disconnected from Ultimate64".to_string(),
                ));
                Command::none()
            }

            Message::RefreshStatus => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Command::none()
                }
            }

            Message::RefreshAfterConnect => {
                // Refresh both status and remote browser after connection
                let status_cmd = if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Command::none()
                };

                let browser_cmd = self
                    .remote_browser
                    .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                    .map(Message::RemoteBrowser);

                Command::batch(vec![status_cmd, browser_cmd])
            }

            Message::StatusUpdated(result) => {
                match result {
                    Ok(status) => {
                        log::info!(
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
                        self.status.connected = false;
                        self.status.device_info = None;
                        self.user_message =
                            Some(UserMessage::Error(format!("Connection failed: {}", e)));
                    }
                }
                Command::none()
            }

            Message::TemplateSelected(template) => {
                self.selected_template = Some(template);
                Command::none()
            }

            Message::ExecuteTemplate => {
                if let Some(template) = &self.selected_template {
                    if let Some(conn) = &self.connection {
                        let conn = conn.clone();
                        let commands = template.commands.clone();
                        return Command::perform(
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
                Command::none()
            }

            Message::ShowError(error) => {
                log::error!("Error: {}", error);
                self.user_message = Some(UserMessage::Error(error));
                Command::none()
            }

            Message::ShowInfo(info) => {
                log::info!("Info: {}", info);
                self.user_message = Some(UserMessage::Info(info));
                Command::none()
            }

            Message::DismissMessage => {
                self.user_message = None;
                Command::none()
            }

            Message::Streaming(msg) => {
                // Handle screenshot result for user message
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

                    return iced::window::change_mode(iced::window::Id::MAIN, mode);
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

                            return Command::perform(
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
                                            Err("Command timed out - device may be offline"
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
                    return Command::none();
                }

                self.video_streaming
                    .update(msg, self.connection.clone())
                    .map(Message::Streaming)
            }

            Message::ExitFullscreen => {
                // Only exit fullscreen if currently in fullscreen mode
                if self.video_streaming.is_fullscreen {
                    self.video_streaming.is_fullscreen = false;
                    return iced::window::change_mode(
                        iced::window::Id::MAIN,
                        iced::window::Mode::Windowed,
                    );
                }
                Command::none()
            }

            Message::ResetMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
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
                    Command::none()
                }
            }

            Message::RebootMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:reboot", host);
                    Command::perform(
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
                    Command::none()
                }
            }

            Message::PauseMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:pause", host);
                    Command::perform(
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
                    Command::none()
                }
            }

            Message::ResumeMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:resume", host);
                    Command::perform(
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
                    Command::none()
                }
            }

            Message::PoweroffMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
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
                    Command::none()
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
                Command::none()
            }

            Message::DefaultSongDurationChanged(value) => {
                if let Ok(duration) = value.parse::<u32>() {
                    if duration > 0 && duration <= 3600 {
                        self.settings.preferences.default_song_duration = duration;
                        self.music_player.set_default_song_duration(duration);
                        // Save settings
                        if let Err(e) = self.settings.save() {
                            log::error!("Failed to save settings: {}", e);
                        }
                    }
                }
                Command::none()
            }

            Message::FontSizeChanged(value) => {
                self.font_size_input = value.clone();
                if let Ok(size) = value.parse::<u32>() {
                    if size >= 8 && size <= 24 {
                        self.settings.preferences.font_size = size;
                        // Save settings
                        if let Err(e) = self.settings.save() {
                            log::error!("Failed to save settings: {}", e);
                        }
                    }
                }
                Command::none()
            }

            // Starting directory settings
            Message::BrowseFileBrowserStartDir => Command::perform(
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
                    self.settings.default_paths.file_browser_start_dir = Some(p);
                    if let Err(e) = self.settings.save() {
                        log::error!("Failed to save settings: {}", e);
                    }
                    self.user_message = Some(UserMessage::Info(
                        "File Browser start directory set (restart app to apply)".to_string(),
                    ));
                }
                Command::none()
            }

            Message::ClearFileBrowserStartDir => {
                self.settings.default_paths.file_browser_start_dir = None;
                if let Err(e) = self.settings.save() {
                    log::error!("Failed to save settings: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "File Browser start directory cleared".to_string(),
                ));
                Command::none()
            }

            Message::BrowseMusicPlayerStartDir => Command::perform(
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
                    self.settings.default_paths.music_player_start_dir = Some(p);
                    if let Err(e) = self.settings.save() {
                        log::error!("Failed to save settings: {}", e);
                    }
                    self.user_message = Some(UserMessage::Info(
                        "Music Player start directory set (restart app to apply)".to_string(),
                    ));
                }
                Command::none()
            }

            Message::ClearMusicPlayerStartDir => {
                self.settings.default_paths.music_player_start_dir = None;
                if let Err(e) = self.settings.save() {
                    log::error!("Failed to save settings: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "Music Player start directory cleared".to_string(),
                ));
                Command::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // If video is in fullscreen mode, show only the fullscreen view
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
                self.tab_button("CONFIG", Tab::Configuration),
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
            Tab::VideoViewer => self.video_streaming.view().map(Message::Streaming),
            Tab::Configuration => self
                .config_editor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::ConfigEditor),
            Tab::Settings => self.view_settings(),
        })
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill);

        // Bottom status/control bar
        let status_bar = self.view_status_bar();

        let main_content = column![
            connection_bar,
            horizontal_rule(1),
            tabs,
            horizontal_rule(1),
            content,
            horizontal_rule(1),
            status_bar
        ]
        .spacing(0);

        container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        use iced::keyboard;

        // Keyboard shortcuts: ESC to exit fullscreen, Opt+F (macOS) or Alt+F (Windows/Linux) to toggle
        let keyboard_sub = keyboard::on_key_press(|key, modifiers| match key {
            keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::ExitFullscreen),
            keyboard::Key::Character(c) if c.as_str() == "f" && modifiers.alt() => Some(
                Message::Streaming(streaming::StreamingMessage::ToggleFullscreen),
            ),
            _ => None,
        });

        Subscription::batch([
            self.video_streaming.subscription().map(Message::Streaming),
            self.music_player.subscription().map(Message::MusicPlayer),
            keyboard_sub,
        ])
    }
}

impl Ultimate64Browser {
    fn tab_button(&self, label: &str, tab: Tab) -> Element<'_, Message> {
        let is_active = self.active_tab == tab;
        button(text(label).size(14))
            .on_press(Message::TabSelected(tab))
            .padding([8, 16])
            .style(if is_active {
                iced::theme::Button::Primary
            } else {
                iced::theme::Button::Secondary
            })
            .into()
    }

    fn view_connection_bar(&self) -> Element<'_, Message> {
        let status_indicator = if self.status.connected {
            text("* CONNECTED").style(iced::theme::Text::Color(iced::Color::from_rgb(
                0.2, 0.8, 0.2,
            )))
        } else {
            text("  DISCONNECTED").style(iced::theme::Text::Color(iced::Color::from_rgb(
                0.8, 0.2, 0.2,
            )))
        };

        let device_text = text(self.status.device_info.as_deref().unwrap_or("No device")).size(12);

        let mounted_text = if !self.status.mounted_disks.is_empty() {
            let disks: Vec<String> = self
                .status
                .mounted_disks
                .iter()
                .map(|(drive, name)| format!("{}:{}", drive.to_uppercase(), name))
                .collect();
            text(disks.join(" | ")).size(12)
        } else {
            text("No disks mounted").size(12)
        };

        // Show Connect button when disconnected, Refresh button when connected
        let action_button: Element<'_, Message> = if self.status.connected {
            button(text("Refresh").size(12))
                .on_press(Message::RefreshStatus)
                .padding([4, 8])
                .into()
        } else {
            button(text("Connect").size(12))
                .on_press(Message::ConnectPressed)
                .padding([4, 8])
                .into()
        };

        container(
            row![
                status_indicator,
                text(" | ").size(12),
                device_text,
                text(" | ").size(12),
                mounted_text,
                horizontal_space(),
            ]
            .push(action_button)
            .spacing(10)
            .align_items(iced::Alignment::Center),
        )
        .padding([5, 10])
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
        .align_items(iced::Alignment::Center);

        let left_pane = container(column![
            left_header,
            horizontal_rule(1),
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
                    "Upload to Ultimate64",
                    tooltip::Position::Right,
                )
                .style(iced::theme::Container::Box),
                Space::with_height(10),
                tooltip(
                    button(text("<<").size(14))
                        .on_press(Message::CopyRemoteToLocal)
                        .padding([8, 12]),
                    "Download from Ultimate64",
                    tooltip::Position::Left,
                )
                .style(iced::theme::Container::Box),
            ]
            .align_items(iced::Alignment::Center),
        )
        .padding([20, 5])
        .center_y();

        // Right pane - Ultimate64 files
        let connection_indicator = if self.status.connected { "*" } else { "" };
        let right_header = row![
            text(format!("ULTIMATE64 {}", connection_indicator))
                .size(12)
                .width(Length::Fill),
            button(text("Refresh").size(10))
                .on_press(Message::RemoteBrowser(RemoteBrowserMessage::RefreshFiles))
                .padding([2, 6]),
        ]
        .padding(5)
        .align_items(iced::Alignment::Center);

        let right_pane = container(column![
            right_header,
            horizontal_rule(1),
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
                button(text("Execute").size(12))
                    .on_press(Message::ExecuteTemplate)
                    .padding([4, 12]),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
        )
        .padding(10);

        column![
            row![left_pane, copy_buttons, right_pane].height(Length::Fill),
            horizontal_rule(1),
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
        let connection_section = column![
            text("CONNECTION SETTINGS").size(18),
            Space::with_height(10),
            text("Ultimate64 IP Address:").size(14),
            text_input("192.168.1.64", &self.host_input)
                .on_input(Message::HostInputChanged)
                .padding(10)
                .width(Length::Fixed(300.0)),
            Space::with_height(10),
            text("Password (optional):").size(14),
            text_input("Enter password...", &self.password_input)
                .on_input(Message::PasswordInputChanged)
                .padding(10)
                .width(Length::Fixed(300.0)),
            Space::with_height(15),
            row![
                button(text("Connect"))
                    .on_press(Message::ConnectPressed)
                    .padding([10, 20]),
                button(text("Disconnect"))
                    .on_press(Message::DisconnectPressed)
                    .padding([10, 20]),
                button(text("Test Connection"))
                    .on_press(Message::RefreshStatus)
                    .padding([10, 20]),
            ]
            .spacing(10),
        ];

        let status_section = column![
            Space::with_height(20),
            horizontal_rule(1),
            Space::with_height(10),
            text("CONNECTION STATUS").size(18),
            Space::with_height(10),
            if self.status.connected {
                text(format!("Connected to {}", self.settings.connection.host)).style(
                    iced::theme::Text::Color(iced::Color::from_rgb(0.2, 0.8, 0.2)),
                )
            } else {
                text("Not connected").style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.8, 0.2, 0.2,
                )))
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
            Space::with_height(20),
            horizontal_rule(1),
            Space::with_height(10),
            text("STARTING DIRECTORIES").size(18),
            Space::with_height(10),
            text("File Browser tab starting directory:").size(14),
            row![
                text(&file_browser_start_dir_display)
                    .size(12)
                    .width(Length::Fixed(400.0)),
                button(text("Browse").size(11))
                    .on_press(Message::BrowseFileBrowserStartDir)
                    .padding([4, 10]),
                button(text("Clear").size(11))
                    .on_press(Message::ClearFileBrowserStartDir)
                    .padding([4, 10]),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
            Space::with_height(10),
            text("Music Player tab starting directory:").size(14),
            row![
                text(&music_player_start_dir_display)
                    .size(12)
                    .width(Length::Fixed(400.0)),
                button(text("Browse").size(11))
                    .on_press(Message::BrowseMusicPlayerStartDir)
                    .padding([4, 10]),
                button(text("Clear").size(11))
                    .on_press(Message::ClearMusicPlayerStartDir)
                    .padding([4, 10]),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
            text("(Changes take effect on next application restart)").size(11),
        ];

        let music_section = column![
            Space::with_height(20),
            horizontal_rule(1),
            Space::with_height(10),
            text("MUSIC PLAYER SETTINGS").size(18),
            Space::with_height(10),
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
            .align_items(iced::Alignment::Center),
        ];

        let ui_section = column![
            Space::with_height(20),
            horizontal_rule(1),
            Space::with_height(10),
            text("UI SETTINGS").size(18),
            Space::with_height(10),
            row![
                text("Font size:").size(14),
                text_input("12", &self.font_size_input)
                    .on_input(Message::FontSizeChanged)
                    .padding(8)
                    .width(Length::Fixed(60.0)),
                text("(8-24, applies to File Browser and Music Player)").size(11),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
        ];

        let debug_section = column![
            Space::with_height(20),
            horizontal_rule(1),
            Space::with_height(10),
            text("DEBUG INFO").size(18),
            text(format!("Platform: {}", std::env::consts::OS)).size(12),
            text(format!("Config dir: {:?}", dirs::config_dir())).size(12),
        ];

        scrollable(container(
            column![
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
            row![
                text(format!("{}{}", prefix, message))
                    .size(12)
                    .style(iced::theme::Text::Color(color)),
                button(text("X").size(10))
                    .on_press(Message::DismissMessage)
                    .padding([2, 6]),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center)
            .into()
        } else {
            text(video_status).size(12).into()
        };

        container(
            row![
                status_text,
                horizontal_space(),
                button(text("PAUSE").size(11))
                    .on_press(Message::PauseMachine)
                    .padding([4, 8]),
                button(text("RESUME").size(11))
                    .on_press(Message::ResumeMachine)
                    .padding([4, 8]),
                text("|").size(12),
                button(text("RESET").size(11))
                    .on_press(Message::ResetMachine)
                    .padding([4, 8]),
                button(text("REBOOT").size(11))
                    .on_press(Message::RebootMachine)
                    .padding([4, 8]),
                button(text("POWER OFF").size(11))
                    .on_press(Message::PoweroffMachine)
                    .padding([4, 8]),
            ]
            .spacing(6)
            .align_items(iced::Alignment::Center),
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
