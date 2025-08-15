#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use iced::{
    executor, widget::{
        button, column, container, horizontal_space, pick_list, row,
        text, text_input, scrollable, horizontal_rule,
        Column, Space,
    }, Application, Command, Element, Length, Settings, Theme,
};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::{Rest, vicstream};
use url::{Host, Url};

mod file_browser;
mod music_player;
mod settings;
mod templates;

use file_browser::{FileBrowser, FileBrowserMessage};
use music_player::{MusicPlayer, MusicPlayerMessage};
use settings::{AppSettings, ConnectionSettings};
use templates::{DiskTemplate, TemplateManager};

pub fn main() -> iced::Result {
    // Initialize logger with debug level for troubleshooting
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    
    log::info!("Starting Ultimate64 Manager");
    Ultimate64Browser::run(Settings::default())
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    FileBrowser(FileBrowserMessage),
    MusicPlayer(MusicPlayerMessage),
    ConnectionChanged(ConnectionSettings),
    HostInputChanged(String),
    PasswordInputChanged(String),
    ConnectPressed,
    DisconnectPressed,
    TemplateSelected(DiskTemplate),
    ExecuteTemplate,
    RefreshStatus,
    StatusUpdated(Result<StatusInfo, String>),
    ShowError(String),
    DismissError,
    // Video viewer messages
    StartVideoStream,
    StopVideoStream,
    TakeScreenshot,
    ScreenshotTaken(Result<String, String>),
    VideoFrameUpdate,
    // Machine control
    ResetMachine,
    PoweroffMachine,
    MachineCommandCompleted(Result<String, String>),
    // Command prompt
    CommandInputChanged(String),
    SendCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    DiskBrowser,
    MusicPlayer,
    VideoViewer,
    Settings,
}

impl std::fmt::Display for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tab::DiskBrowser => write!(f, "Disk Browser"),
            Tab::MusicPlayer => write!(f, "Music Player"),
            Tab::VideoViewer => write!(f, "Video Viewer"),
            Tab::Settings => write!(f, "Settings"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub connected: bool,
    pub device_info: Option<String>,
    pub mounted_disks: Vec<(String, String)>, // (drive, filename)
}

pub struct Ultimate64Browser {
    active_tab: Tab,
    file_browser: FileBrowser,
    music_player: MusicPlayer,
    settings: AppSettings,
    template_manager: TemplateManager,
    selected_template: Option<DiskTemplate>,
    connection: Option<Arc<TokioMutex<Rest>>>,
    status: StatusInfo,
    error_message: Option<String>,
    // Input fields for settings
    host_input: String,
    password_input: String,
    // Video streaming - improved with proper thread management
    video_streaming: bool,
    video_frame: Arc<Mutex<Option<Vec<u8>>>>,
    video_stream_handle: Option<thread::JoinHandle<()>>,
    video_stop_signal: Arc<AtomicBool>,
    // Command prompt
    command_input: String,
    command_history: Vec<String>,
}

impl Application for Ultimate64Browser {
    type Message = Message;
    type Theme = Theme;
    type Executor = executor::Default;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        let settings = AppSettings::load().unwrap_or_default();
        let mut app = Self {
            active_tab: Tab::DiskBrowser,
            file_browser: FileBrowser::new(),
            music_player: MusicPlayer::new(),
            host_input: settings.connection.host.clone(),
            password_input: settings.connection.password.clone().unwrap_or_default(),
            settings: settings.clone(),
            template_manager: TemplateManager::new(),
            selected_template: None,
            connection: None,
            status: StatusInfo {
                connected: false,
                device_info: None,
                mounted_disks: Vec::new(),
            },
            error_message: None,
            video_streaming: false,
            video_frame: Arc::new(Mutex::new(None)),
            video_stream_handle: None,
            video_stop_signal: Arc::new(AtomicBool::new(false)),
            command_input: String::new(),
            command_history: Vec::new(),
        };

        // Try to establish connection if settings are available
        if !settings.connection.host.is_empty() {
            app.establish_connection();
            // Return a command to refresh status after a brief delay to allow connection to establish
            return (app, Command::perform(
                async {
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                },
                |_| Message::RefreshStatus
            ));
        }

        (app, Command::none())
    }

    fn title(&self) -> String {
        String::from("Ultimate64 Manager")
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::TabSelected(tab) => {
                self.active_tab = tab;
                Command::none()
            }
            Message::FileBrowser(msg) => {
                // Check if it's a mount completion message
                let should_refresh = matches!(msg, FileBrowserMessage::MountCompleted(Ok(_)));
                
                let cmd = self.file_browser.update(msg, self.connection.clone())
                    .map(Message::FileBrowser);
                
                // If mount was successful, also refresh status
                if should_refresh {
                    Command::batch(vec![
                        cmd,
                        Command::perform(
                            async {
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            },
                            |_| Message::RefreshStatus
                        ),
                    ])
                } else {
                    cmd
                }
            }
            Message::MusicPlayer(msg) => {
                self.music_player.update(msg, self.connection.clone())
                    .map(Message::MusicPlayer)
            }
            Message::HostInputChanged(value) => {
                self.host_input = value;
                Command::none()
            }
            Message::PasswordInputChanged(value) => {
                self.password_input = value;
                Command::none()
            }
            Message::ConnectPressed => {
                let conn_settings = ConnectionSettings {
                    host: self.host_input.clone(),
                    password: if self.password_input.is_empty() {
                        None
                    } else {
                        Some(self.password_input.clone())
                    },
                };
                self.settings.connection = conn_settings;
                self.settings.save().ok();
                self.establish_connection();
                // Automatically refresh status after connecting
                Command::perform(
                    async {
                        // Small delay to ensure connection is established
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    },
                    |_| Message::RefreshStatus
                )
            }
            Message::DisconnectPressed => {
                self.connection = None;
                self.status.connected = false;
                self.status.device_info = None;
                self.status.mounted_disks.clear();
                log::info!("Disconnected from Ultimate64");
                Command::none()
            }
            Message::CommandInputChanged(input) => {
                self.command_input = input;
                Command::none()
            }
            Message::SendCommand => {
                if !self.command_input.is_empty() {
                    let cmd = self.command_input.clone();
                    self.command_history.push(cmd.clone());
                    self.command_input.clear();
                    
                    if let Some(conn) = &self.connection {
                        let conn = conn.clone();
                        Command::perform(
                            async move {
                                let conn = conn.lock().await;
                                conn.type_text(&format!("{}\n", cmd))
                                    .map(|_| format!("Sent: {}", cmd))
                                    .map_err(|e| format!("Failed to send command: {}", e))
                            },
                            Message::MachineCommandCompleted,
                        )
                    } else {
                        self.error_message = Some("Not connected to Ultimate64".to_string());
                        Command::none()
                    }
                } else {
                    Command::none()
                }
            }
            Message::ConnectionChanged(conn) => {
                self.settings.connection = conn;
                self.settings.save().ok();
                self.establish_connection();
                Command::perform(
                    async {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    },
                    |_| Message::RefreshStatus
                )
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
                            async move {
                                execute_template_commands(conn, commands).await
                            },
                            |result| match result {
                                Ok(_) => Message::RefreshStatus,
                                Err(e) => Message::ShowError(e),
                            },
                        );
                    }
                }
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
            Message::StatusUpdated(result) => {
                match result {
                    Ok(status) => {
                        log::info!("Status updated - Connected: {}, Device: {:?}, Disks: {}", 
                            status.connected, status.device_info, status.mounted_disks.len());
                        self.status = status;
                    }
                    Err(e) => {
                        log::error!("Status update failed: {}", e);
                        self.error_message = Some(e);
                    }
                }
                Command::none()
            }
            Message::ShowError(error) => {
                self.error_message = Some(error);
                Command::none()
            }
            Message::DismissError => {
                self.error_message = None;
                Command::none()
            }
            Message::StartVideoStream => {
                if !self.video_streaming {
                    self.start_video_stream();
                }
                Command::none()
            }
            Message::StopVideoStream => {
                self.stop_video_stream();
                Command::none()
            }
            Message::TakeScreenshot => {
                Command::perform(
                    take_screenshot_async(),
                    Message::ScreenshotTaken,
                )
            }
            Message::ScreenshotTaken(result) => {
                match result {
                    Ok(path) => {
                        log::info!("Screenshot saved to: {}", path);
                        self.error_message = Some(format!("Screenshot saved to: {}", path));
                    }
                    Err(e) => {
                        log::error!("Screenshot failed: {}", e);
                        self.error_message = Some(format!("Screenshot failed: {}", e));
                    }
                }
                Command::none()
            }
            Message::VideoFrameUpdate => {
                // Trigger a redraw when we have a new frame
                Command::none()
            }
            Message::ResetMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
                        async move {
                            let conn = conn.lock().await;
                            conn.reset()
                                .map(|_| "Machine reset successfully".to_string())
                                .map_err(|e| format!("Reset failed: {}", e))
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.error_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
                }
            }
            Message::PoweroffMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Command::perform(
                        async move {
                            let conn = conn.lock().await;
                            conn.poweroff()
                                .map(|_| "Machine powered off".to_string())
                                .map_err(|e| format!("Poweroff failed: {}", e))
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.error_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
                }
            }
            Message::MachineCommandCompleted(result) => {
                match result {
                    Ok(msg) => {
                        log::info!("{}", msg);
                        // Don't show success as error, maybe add a status message later
                    }
                    Err(e) => {
                        self.error_message = Some(e);
                    }
                }
                Command::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // Tab bar
        let tabs = container(
            row![
                button(text(if self.active_tab == Tab::DiskBrowser { "▼ DISK BROWSER" } else { "DISK BROWSER" }))
                    .on_press(Message::TabSelected(Tab::DiskBrowser))
                    .padding([10, 20]),
                button(text(if self.active_tab == Tab::MusicPlayer { "▼ MUSIC PLAYER" } else { "MUSIC PLAYER" }))
                    .on_press(Message::TabSelected(Tab::MusicPlayer))
                    .padding([10, 20]),
                button(text(if self.active_tab == Tab::VideoViewer { "▼ VIDEO VIEWER" } else { "VIDEO VIEWER" }))
                    .on_press(Message::TabSelected(Tab::VideoViewer))
                    .padding([10, 20]),
                button(text(if self.active_tab == Tab::Settings { "▼ SETTINGS" } else { "SETTINGS" }))
                    .on_press(Message::TabSelected(Tab::Settings))
                    .padding([10, 20]),
            ]
            .spacing(2),
        )
        .padding(5);

        let status_bar = container(self.view_status_bar())
            .padding(10)
            .width(Length::Fill);

        let content = container(
            match self.active_tab {
                Tab::DiskBrowser => self.view_disk_browser(),
                Tab::MusicPlayer => self.view_music_player(),
                Tab::VideoViewer => self.view_video_viewer(),
                Tab::Settings => self.view_settings(),
            }
        )
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill);

        let main_content = column![
            tabs,
            horizontal_rule(1),
            content,
            horizontal_rule(1),
            status_bar
        ]
        .spacing(0);

        // Show error overlay if there's an error
        if let Some(error) = &self.error_message {
            let error_box = container(
                column![
                    text("ERROR").size(24),
                    text(error).size(14),
                    Space::with_height(10),
                    button(text("DISMISS"))
                        .on_press(Message::DismissError)
                        .padding(10),
                ]
                .spacing(10)
                .padding(30),
            )
            .max_width(400)
            .center_x()
            .center_y();

            container(column![main_content, error_box])
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            container(main_content)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }
}

impl Ultimate64Browser {
    fn establish_connection(&mut self) {
        // Clear old connection first
        self.connection = None;
        self.status.connected = false;
        self.status.device_info = None;
        self.status.mounted_disks.clear();
        
        if self.settings.connection.host.is_empty() {
            self.error_message = Some("Host IP cannot be empty".to_string());
            return;
        }

        // Parse the host - try as IP address first
        let host = if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv4Addr>() {
            Host::Ipv4(ip_addr)
        } else if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv6Addr>() {
            Host::Ipv6(ip_addr)
        } else {
            // Try as domain name
            match Host::parse(&self.settings.connection.host) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("Invalid host: {}", e);
                    self.error_message = Some(format!("Invalid host address: {}", e));
                    self.status.connected = false;
                    self.connection = None;
                    return;
                }
            }
        };

        log::info!("Attempting to connect to {:?} with password: {}", 
            host, 
            self.settings.connection.password.is_some());
        
        match Rest::new(&host, self.settings.connection.password.clone()) {
            Ok(rest) => {
                log::info!("Connection established successfully");
                self.connection = Some(Arc::new(TokioMutex::new(rest)));
                self.status.connected = true;
                log::info!("Connected successfully to Ultimate64!");
            }
            Err(e) => {
                log::error!("Connection failed: {}", e);
                self.error_message = Some(format!("Connection failed: {}", e));
                self.status.connected = false;
                self.connection = None;
            }
        }
    }

    fn start_video_stream(&mut self) {
        if self.video_streaming {
            return;
        }

        log::info!("Starting video stream...");
        
        // Reset the stop signal
        self.video_stop_signal.store(false, Ordering::Relaxed);
        
        let frame_buffer = self.video_frame.clone();
        let stop_signal = self.video_stop_signal.clone();
        self.video_streaming = true;

        // Start background thread to capture VIC frames with proper shutdown
        let handle = thread::spawn(move || {
            log::info!("Video stream thread started");
            let url = Url::parse("udp://239.0.1.64:11000").unwrap();
            
            match vicstream::get_socket(&url) {
                Ok(socket) => {
                    // Set socket to non-blocking mode for responsive shutdown
                    if let Err(e) = socket.set_nonblocking(true) {
                        log::error!("Failed to set socket to non-blocking: {:?}", e);
                        return;
                    }

                    log::info!("Video stream socket created and set to non-blocking");

                    loop {
                        // Check if we should stop
                        if stop_signal.load(Ordering::Relaxed) {
                            log::info!("Video stream thread received stop signal");
                            break;
                        }

                        match vicstream::capture_frame(socket.try_clone().unwrap()) {
                            Ok(data) => {
                                // Store the raw frame data
                                if let Ok(mut frame) = frame_buffer.lock() {
                                    *frame = Some(data);
                                } else {
                                    log::warn!("Failed to lock frame buffer");
                                }
                            }
                            Err(e) => {
                                // Handle would-block errors (expected with non-blocking socket)
                                if let Some(io_error) = e.downcast_ref::<std::io::Error>() {
                                    if io_error.kind() == std::io::ErrorKind::WouldBlock {
                                        // No data available, sleep briefly and continue
                                        thread::sleep(std::time::Duration::from_millis(10));
                                        continue;
                                    }
                                }
                                
                                log::error!("Frame capture error: {:?}", e);
                                thread::sleep(std::time::Duration::from_millis(50));
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to create UDP socket: {:?}", e);
                }
            }
            
            log::info!("Video stream thread terminated gracefully");
        });

        self.video_stream_handle = Some(handle);
        log::info!("Video stream started successfully");
    }

    fn stop_video_stream(&mut self) {
        if !self.video_streaming {
            return;
        }

        log::info!("Stopping video stream...");
        
        // Signal the thread to stop
        self.video_stop_signal.store(true, Ordering::Relaxed);
        
        // Wait for the thread to finish (with timeout)
        if let Some(handle) = self.video_stream_handle.take() {
            // Create a separate thread to handle the join with timeout
            let join_handle = thread::spawn(move || {
                handle.join()
            });
            
            // Give the thread up to 2 seconds to shut down gracefully
            let timeout = std::time::Duration::from_secs(2);
            let start = std::time::Instant::now();
            let mut joined = false;
            
            while start.elapsed() < timeout {
                if join_handle.is_finished() {
                    match join_handle.join() {
                        Ok(Ok(())) => {
                            log::info!("Video stream thread joined successfully");
                            joined = true;
                        },
                        Ok(Err(e)) => {
                            log::warn!("Video stream thread panicked: {:?}", e);
                            joined = true;
                        },
                        Err(_) => {
                            log::warn!("Failed to join video stream thread");
                            joined = true;
                        }
                    }
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            
            if !joined {
                log::warn!("Video stream thread did not shut down within timeout - continuing anyway");
            }
        }
        
        self.video_streaming = false;
        
        // Clear the frame buffer
        if let Ok(mut frame) = self.video_frame.lock() {
            *frame = None;
        }
        
        log::info!("Video stream stopped");
    }

    fn view_video_viewer(&self) -> Element<'_, Message> {
        // Control buttons
        let controls = row![
            if self.video_streaming {
                button(text("■ STOP STREAM"))
                    .on_press(Message::StopVideoStream)
                    .padding(10)
            } else {
                button(text("▶ START STREAM"))
                    .on_press(Message::StartVideoStream)
                    .padding(10)
            },
            button(text("📷 SCREENSHOT"))
                .on_press(Message::TakeScreenshot)
                .padding(10),
        ]
        .spacing(10);

        let video_display: Element<'_, Message> = if self.video_streaming {
            if let Ok(frame_guard) = self.video_frame.lock() {
                if let Some(frame_data) = &*frame_guard {
                    container(
                        column![
                            text("STREAMING ACTIVE").size(16),
                            text(format!("{} bytes received", frame_data.len())),
                            text("384x272 @ 50Hz"),
                            text("UDP: 239.0.1.64:11000"),
                        ]
                        .spacing(5)
                        .align_items(iced::Alignment::Center),
                    )
                    .padding(20)
                    .width(Length::Fill)
                    .into()
                } else {
                    container(
                        column![
                            text("STREAMING ACTIVE").size(16),
                            text("Waiting for video frames..."),
                            text("UDP: 239.0.1.64:11000"),
                        ]
                        .spacing(5)
                        .align_items(iced::Alignment::Center),
                    )
                    .padding(20)
                    .width(Length::Fill)
                    .into()
                }
            } else {
                text("Error accessing frame buffer").into()
            }
        } else {
            container(
                column![
                    text("VIDEO STREAM INACTIVE").size(16),
                    Space::with_height(10),
                    text("UDP MULTICAST"),
                    text("239.0.1.64:11000"),
                    Space::with_height(10),
                    text("Click START STREAM to begin"),
                ]
                .spacing(5)
                .align_items(iced::Alignment::Center),
            )
            .padding(40)
            .width(Length::Fill)
            .into()
        };

        // Command history display
        let history_display: Element<'_, Message> = if !self.command_history.is_empty() {
            let history_text: Vec<Element<'_, Message>> = self.command_history
                .iter()
                .rev()
                .take(10)
                .map(|cmd| {
                    text(format!("> {}", cmd))
                        .size(11)
                        .into()
                })
                .collect();
            
            container(
                scrollable(
                    Column::with_children(history_text)
                        .spacing(2)
                        .padding(10),
                )
                .height(Length::Fixed(150.0)),
            )
            .width(Length::Fill)
            .into()
        } else {
            container(
                text("Command history will appear here")
                    .size(11),
            )
            .padding(10)
            .height(Length::Fixed(150.0))
            .width(Length::Fill)
            .into()
        };

        // Command prompt
        let command_prompt = container(
            row![
                text("C64>"),
                text_input("Enter BASIC command...", &self.command_input)
                    .on_input(Message::CommandInputChanged)
                    .on_submit(Message::SendCommand),
                button(text("SEND"))
                    .on_press(Message::SendCommand)
                    .padding(5),
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
        )
        .padding(10)
        .width(Length::Fill);

        column![
            text("VIC VIDEO STREAM").size(20),
            horizontal_rule(1),
            Space::with_height(10),
            controls,
            Space::with_height(10),
            video_display,
            Space::with_height(10),
            text("COMMAND HISTORY").size(14),
            history_display,
            Space::with_height(10),
            text("COMMAND PROMPT").size(14),
            command_prompt,
        ]
        .spacing(10)
        .into()
    }

    fn view_disk_browser(&self) -> Element<'_, Message> {
        let browser = self.file_browser.view().map(Message::FileBrowser);

        let template_section = column![
            text("Quick Actions").size(18),
            row![
                pick_list(
                    self.template_manager.get_templates(),
                    self.selected_template.clone(),
                    Message::TemplateSelected,
                )
                .placeholder("Select template...")
                .width(Length::FillPortion(2)),
                button(text("Execute"))
                    .on_press(Message::ExecuteTemplate)
                    .padding(10),
            ]
            .spacing(10),
        ]
        .spacing(10);

        column![browser, template_section]
            .spacing(20)
            .padding(10)
            .into()
    }

    fn view_music_player(&self) -> Element<'_, Message> {
        self.music_player.view().map(Message::MusicPlayer)
    }

    fn view_settings(&self) -> Element<'_, Message> {
        let host_input = text_input("192.168.1.64", &self.host_input)
            .on_input(Message::HostInputChanged)
            .padding(10);
        
        let password_input = text_input("Optional password", &self.password_input)
            .on_input(Message::PasswordInputChanged)
            .padding(10);

        let connect_button = button(text("Connect"))
            .on_press(Message::ConnectPressed)
            .padding(10);
        
        let disconnect_button = button(text("Disconnect"))
            .on_press(Message::DisconnectPressed)
            .padding(10);

        let test_button = button(text("Test Connection"))
            .on_press(Message::RefreshStatus)
            .padding(10);

        column![
            text("Connection Settings").size(24),
            text("Ultimate64 Host IP:"),
            host_input,
            text("Password (optional - will be visible):"),
            password_input,
            row![
                connect_button,
                disconnect_button,
                test_button,
            ].spacing(10),
            text(""),
            text("Connection Status:"),
            if self.status.connected {
                text(format!("[OK] Connected to {}", self.settings.connection.host))
            } else {
                text("[--] Not connected")
            },
            if let Some(info) = &self.status.device_info {
                text(format!("Device: {}", info))
            } else {
                text("")
            },
        ]
        .spacing(10)
        .padding(20)
        .into()
    }

    fn view_status_bar(&self) -> Element<'_, Message> {
        let connection_indicator = text(if self.status.connected { 
            "● CONNECTED" 
        } else { 
            "○ DISCONNECTED" 
        });

        let device_info = text(
            self.status.device_info.as_deref().unwrap_or("NO DEVICE")
        );

        let mounted_info = text(if !self.status.mounted_disks.is_empty() {
            let disks: Vec<String> = self.status.mounted_disks
                .iter()
                .map(|(drive, name)| format!("[{}] {}", drive.to_uppercase(), name))
                .collect();
            disks.join(" ")
        } else {
            "NO DISKS".to_string()
        });

        let video_status = text(if self.video_streaming {
            "📹 STREAMING"
        } else {
            "📹 IDLE"
        });

        row![
            connection_indicator,
            text(" | "),
            device_info,
            text(" | "),
            mounted_info,
            text(" | "),
            video_status,
            horizontal_space(),
            button(text("RESET"))
                .on_press(Message::ResetMachine)
                .padding(5),
            button(text("POWER"))
                .on_press(Message::PoweroffMachine)
                .padding(5),
            button(text("REFRESH"))
                .on_press(Message::RefreshStatus)
                .padding(5),
        ]
        .spacing(10)
        .align_items(iced::Alignment::Center)
        .into()
    }
}

// Implement Drop to ensure proper cleanup
impl Drop for Ultimate64Browser {
    fn drop(&mut self) {
        log::info!("Ultimate64Browser is being dropped, cleaning up...");
        
        // Ensure video stream is stopped when the app is dropped
        if self.video_streaming {
            log::info!("Stopping video stream during cleanup...");
            self.stop_video_stream();
        }
        
        log::info!("Ultimate64Browser cleanup completed");
    }
}

async fn take_screenshot_async() -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    log::info!("Taking screenshot...");
    
    // Create URL for VIC stream
    let url = Url::parse("udp://239.0.1.64:11000").map_err(|e| e.to_string())?;
    
    // Generate filename with timestamp
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    
    let filename = format!("screenshot_{}.png", timestamp);
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(&filename);
    
    // Take screenshot using the vicstream module
    vicstream::take_snapshot(&url, Some(&path), Some(2))
        .map_err(|e| e.to_string())?;
    
    Ok(path.to_string_lossy().to_string())
}

async fn execute_template_commands(
    connection: Arc<TokioMutex<Rest>>,
    commands: Vec<String>,
) -> Result<(), String> {
    let conn = connection.lock().await;
    
    log::info!("Executing template with {} commands", commands.len());
    
    for command in commands {
        log::info!("Executing command: {}", command);
        
        // Parse and execute template commands
        if command.starts_with("RESET") {
            log::info!("Sending reset command...");
            conn.reset().map_err(|e| {
                log::error!("Reset failed: {}", e);
                format!("Reset failed: {}", e)
            })?;
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        } else if let Some(text) = command.strip_prefix("TYPE ") {
            log::info!("Typing text: {}", text);
            conn.type_text(text).map_err(|e| {
                log::error!("Type text failed: {}", e);
                format!("Type text failed: {}", e)
            })?;
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("LOAD") {
            log::info!("Sending load command...");
            // Use the same format as the CLI example: load "*",8,1
            conn.type_text("load\"*\",8,1\n").map_err(|e| {
                log::error!("Load command failed: {}", e);
                format!("Load command failed: {}", e)
            })?;
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("RUN") {
            log::info!("Sending run command...");
            conn.type_text("run\n").map_err(|e| {
                log::error!("Run command failed: {}", e);
                format!("Run command failed: {}", e)
            })?;
        } else {
            log::warn!("Unknown command: {}", command);
        }
    }
    
    log::info!("Template execution completed successfully");
    Ok(())
}

async fn fetch_status(connection: Arc<TokioMutex<Rest>>) -> Result<StatusInfo, String> {
    let conn = connection.lock().await;
    
    log::info!("Fetching device status...");
    
    let device_info = match conn.info() {
        Ok(info) => {
            log::info!("Got device info: {} ({})", info.product, info.firmware_version);
            Some(format!("{} ({})", info.product, info.firmware_version))
        }
        Err(e) => {
            log::error!("Failed to get device info: {}", e);
            return Err(format!("Failed to get device info: {}", e));
        }
    };
    
    let mounted_disks = match conn.drive_list() {
        Ok(drives) => {
            log::info!("Got drive list with {} drives", drives.len());
            drives.into_iter()
                .filter_map(|(name, drive)| {
                    drive.image_file.map(|file| (name, file))
                })
                .collect()
        }
        Err(e) => {
            log::warn!("Failed to get drive list: {}", e);
            Vec::new()
        }
    };

    Ok(StatusInfo {
        connected: true,
        device_info,
        mounted_disks,
    })
}