use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::{
    Command, Element, Length, Subscription,
    widget::{
        Column, Space, button, checkbox, column, container, image as iced_image, mouse_area, row,
        scrollable, text, text_input, tooltip,
    },
};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Video frame dimensions
pub const VIC_WIDTH: u32 = 384;
pub const VIC_HEIGHT: u32 = 272;

// Audio constants
const AUDIO_PORT_OFFSET: u16 = 1; // Audio port = video port + 1
const AUDIO_SAMPLE_RATE: u32 = 48000;
const AUDIO_CHANNELS: u16 = 2;
const AUDIO_HEADER_SIZE: usize = 2; // Just sequence number
const AUDIO_BUFFER_SIZE: usize = AUDIO_SAMPLE_RATE as usize; // ~1 second buffer

// Ultimate64 video packet header (12 bytes)
const HEADER_SIZE: usize = 12;

// C64 color palette (RGB values) - from u64view
const C64_PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0: Black
    [0xFF, 0xFF, 0xFF], // 1: White
    [0x68, 0x37, 0x2B], // 2: Red
    [0x70, 0xA4, 0xB2], // 3: Cyan
    [0x6F, 0x3D, 0x86], // 4: Purple
    [0x58, 0x8D, 0x43], // 5: Green
    [0x35, 0x28, 0x79], // 6: Blue
    [0xB8, 0xC7, 0x6F], // 7: Yellow
    [0x6F, 0x4F, 0x25], // 8: Orange
    [0x43, 0x39, 0x00], // 9: Brown
    [0x9A, 0x67, 0x59], // 10: Light Red
    [0x44, 0x44, 0x44], // 11: Dark Grey
    [0x6C, 0x6C, 0x6C], // 12: Grey
    [0x9A, 0xD2, 0x84], // 13: Light Green
    [0x6C, 0x5E, 0xB5], // 14: Light Blue
    [0x95, 0x95, 0x95], // 15: Light Grey
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    Unicast,   // Direct UDP to this machine
    Multicast, // UDP multicast 239.0.1.64
}

impl std::fmt::Display for StreamMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamMode::Unicast => write!(f, "Unicast (Direct IP)"),
            StreamMode::Multicast => write!(f, "Multicast"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StreamingMessage {
    StartStream,
    StopStream,
    FrameUpdate,
    TakeScreenshot,
    ScreenshotComplete(Result<String, String>),
    CommandInputChanged(String),
    SendCommand,
    CommandSent(Result<String, String>),
    StreamModeChanged(StreamMode),
    PortChanged(String),
    AudioToggled(bool),
    ToggleFullscreen,
    VideoClicked, // For double-click detection
}

pub struct VideoStreaming {
    pub is_streaming: bool,
    pub frame_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub image_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub stop_signal: Arc<AtomicBool>,
    stream_handle: Option<thread::JoinHandle<()>>,
    audio_stream_handle: Option<thread::JoinHandle<()>>,
    audio_network_handle: Option<thread::JoinHandle<()>>,
    pub command_input: String,
    pub command_history: Vec<String>,
    pub stream_mode: StreamMode,
    pub listen_port: String,
    pub packets_received: Arc<Mutex<u64>>,
    pub audio_packets_received: Arc<Mutex<u64>>,
    pub audio_enabled: bool,
    audio_buffer: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub is_fullscreen: bool,
    last_click_time: Option<std::time::Instant>,
}

impl Default for VideoStreaming {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoStreaming {
    pub fn new() -> Self {
        Self {
            is_streaming: false,
            frame_buffer: Arc::new(Mutex::new(None)),
            image_buffer: Arc::new(Mutex::new(None)),
            stop_signal: Arc::new(AtomicBool::new(false)),
            stream_handle: None,
            audio_stream_handle: None,
            audio_network_handle: None,
            command_input: String::new(),
            command_history: Vec::new(),
            stream_mode: StreamMode::Unicast,
            listen_port: "11000".to_string(),
            packets_received: Arc::new(Mutex::new(0)),
            audio_packets_received: Arc::new(Mutex::new(0)),
            audio_enabled: true,
            audio_buffer: None,
            is_fullscreen: false,
            last_click_time: None,
        }
    }

    pub fn update(&mut self, message: StreamingMessage) -> Command<StreamingMessage> {
        match message {
            StreamingMessage::StartStream => {
                self.start_stream();
                Command::none()
            }
            StreamingMessage::StopStream => {
                self.stop_stream();
                Command::none()
            }
            StreamingMessage::FrameUpdate => {
                // Frame buffer now contains RGBA data directly - just copy to image buffer
                if let Ok(frame_guard) = self.frame_buffer.lock() {
                    if let Some(rgba_data) = &*frame_guard {
                        if let Ok(mut img_guard) = self.image_buffer.lock() {
                            *img_guard = Some(rgba_data.clone());
                        }
                    }
                }
                Command::none()
            }
            StreamingMessage::TakeScreenshot => {
                // Take screenshot from the existing image buffer
                if !self.is_streaming {
                    return Command::none();
                }

                // Get current frame from buffer
                let rgba_data = if let Ok(img_guard) = self.image_buffer.lock() {
                    img_guard.clone()
                } else {
                    None
                };

                if let Some(data) = rgba_data {
                    Command::perform(
                        save_screenshot_to_pictures(data),
                        StreamingMessage::ScreenshotComplete,
                    )
                } else {
                    Command::perform(
                        async { Err("No frame available".to_string()) },
                        StreamingMessage::ScreenshotComplete,
                    )
                }
            }
            StreamingMessage::ScreenshotComplete(_result) => {
                // Handled by main app for user message display
                Command::none()
            }
            StreamingMessage::CommandInputChanged(value) => {
                self.command_input = value;
                Command::none()
            }
            StreamingMessage::SendCommand => {
                // Handled by main.rs which has access to the Rest connection
                Command::none()
            }
            StreamingMessage::CommandSent(result) => {
                match result {
                    Ok(msg) => self.command_history.push(msg),
                    Err(e) => self.command_history.push(format!("Error: {}", e)),
                }
                Command::none()
            }
            StreamingMessage::StreamModeChanged(mode) => {
                self.stream_mode = mode;
                Command::none()
            }
            StreamingMessage::PortChanged(port) => {
                self.listen_port = port;
                Command::none()
            }
            StreamingMessage::AudioToggled(enabled) => {
                self.audio_enabled = enabled;
                Command::none()
            }
            StreamingMessage::ToggleFullscreen => {
                // Note: This is handled by main.rs which changes window mode
                Command::none()
            }
            StreamingMessage::VideoClicked => {
                // Check for double-click (within 300ms)
                let now = std::time::Instant::now();
                if let Some(last_time) = self.last_click_time {
                    if now.duration_since(last_time).as_millis() < 300 {
                        // Double-click detected
                        self.last_click_time = None;
                        return Command::perform(async {}, |_| StreamingMessage::ToggleFullscreen);
                    }
                }
                self.last_click_time = Some(now);
                Command::none()
            }
        }
    }

    /// Fullscreen view - video fills the entire available space with black letterboxing
    pub fn view_fullscreen(&self) -> Element<'_, StreamingMessage> {
        let video_content: Element<'_, StreamingMessage> = if self.is_streaming {
            if let Ok(img_guard) = self.image_buffer.lock() {
                if let Some(rgba_data) = &*img_guard {
                    let handle = iced::widget::image::Handle::from_pixels(
                        VIC_WIDTH,
                        VIC_HEIGHT,
                        rgba_data.clone(),
                    );

                    mouse_area(
                        iced_image(handle)
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .content_fit(iced::ContentFit::Contain),
                    )
                    .on_press(StreamingMessage::VideoClicked)
                    .into()
                } else {
                    text("Waiting for frames...")
                        .size(20)
                        .style(iced::theme::Text::Color(iced::Color::WHITE))
                        .into()
                }
            } else {
                text("Frame buffer error")
                    .size(20)
                    .style(iced::theme::Text::Color(iced::Color::WHITE))
                    .into()
            }
        } else {
            text("Stream not active - press ESC to exit")
                .size(20)
                .style(iced::theme::Text::Color(iced::Color::WHITE))
                .into()
        };

        // Exit hint at the top
        let exit_hint = container(
            button(text("Exit Fullscreen (ESC or double-click)").size(12))
                .on_press(StreamingMessage::ToggleFullscreen)
                .padding([6, 12]),
        )
        .width(Length::Fill)
        .center_x()
        .padding(10);

        // Black background container with centered video
        container(column![
            exit_hint,
            container(video_content)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x()
                .center_y(),
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(iced::theme::Container::Custom(Box::new(BlackBackground)))
        .into()
    }

    pub fn view(&self) -> Element<'_, StreamingMessage> {
        // Video packets info
        let video_packets = self.packets_received.lock().map(|p| *p).unwrap_or(0);
        let audio_packets = self.audio_packets_received.lock().map(|p| *p).unwrap_or(0);

        // === LEFT SIDE: Video display ===
        let video_display: Element<'_, StreamingMessage> = if self.is_streaming {
            // Try to display the decoded RGBA image
            if let Ok(img_guard) = self.image_buffer.lock() {
                if let Some(rgba_data) = &*img_guard {
                    // Create an image handle from RGBA data
                    let handle = iced::widget::image::Handle::from_pixels(
                        VIC_WIDTH,
                        VIC_HEIGHT,
                        rgba_data.clone(),
                    );

                    // Wrap image in mouse_area for double-click fullscreen
                    let video_image = mouse_area(
                        iced_image(handle)
                            .width(Length::Fixed((VIC_WIDTH * 2) as f32))
                            .height(Length::Fixed((VIC_HEIGHT * 2) as f32))
                            .content_fit(iced::ContentFit::Fill),
                    )
                    .on_press(StreamingMessage::VideoClicked);

                    container(
                        column![
                            video_image,
                            text(format!(
                                "384x272 | Video: {} | Audio: {} | Double-click for fullscreen",
                                video_packets, audio_packets
                            ))
                            .size(10),
                        ]
                        .spacing(5)
                        .align_items(iced::Alignment::Center),
                    )
                    .padding(10)
                    .into()
                } else {
                    // Image not decoded yet, show raw frame info
                    if let Ok(frame_guard) = self.frame_buffer.lock() {
                        if let Some(frame_data) = &*frame_guard {
                            container(
                                column![
                                    text("RECEIVING FRAMES").size(16),
                                    text(format!("{} bytes", frame_data.len())).size(12),
                                    text(format!(
                                        "Video: {} | Audio: {}",
                                        video_packets, audio_packets
                                    ))
                                    .size(12),
                                ]
                                .spacing(5)
                                .align_items(iced::Alignment::Center),
                            )
                            .padding(40)
                            .into()
                        } else {
                            container(
                                column![
                                    text("Waiting for frames...").size(14),
                                    text(format!(
                                        "Video: {} | Audio: {}",
                                        video_packets, audio_packets
                                    ))
                                    .size(12),
                                ]
                                .spacing(5)
                                .align_items(iced::Alignment::Center),
                            )
                            .padding(40)
                            .into()
                        }
                    } else {
                        container(text("Waiting for frames...").size(14))
                            .padding(40)
                            .into()
                    }
                }
            } else {
                text("Frame buffer error").into()
            }
        } else {
            let status_info = match self.stream_mode {
                StreamMode::Unicast => format!(
                    "Unicast mode: Configure Ultimate64 to send to YOUR_IP:{}",
                    self.listen_port
                ),
                StreamMode::Multicast => {
                    "Multicast mode: 239.0.1.64 (requires wired LAN)".to_string()
                }
            };

            container(
                column![
                    text("VIDEO STREAM INACTIVE").size(16),
                    Space::with_height(10),
                    text(&status_info).size(11),
                    Space::with_height(5),
                    text("Click START to begin streaming").size(11),
                ]
                .align_items(iced::Alignment::Center),
            )
            .width(Length::Fixed((VIC_WIDTH * 2) as f32))
            .height(Length::Fixed((VIC_HEIGHT * 2) as f32))
            .center_x()
            .center_y()
            .into()
        };

        // === RIGHT SIDE: Controls panel ===

        // Mode selection
        let mode_section = column![
            text("Stream Mode").size(12),
            row![
                tooltip(
                    button(text("Unicast").size(11))
                        .on_press(StreamingMessage::StreamModeChanged(StreamMode::Unicast))
                        .padding([4, 8])
                        .style(if self.stream_mode == StreamMode::Unicast {
                            iced::theme::Button::Primary
                        } else {
                            iced::theme::Button::Secondary
                        }),
                    "Direct IP connection (works over WiFi)",
                    tooltip::Position::Bottom,
                )
                .style(iced::theme::Container::Box),
                tooltip(
                    button(text("Multicast").size(11))
                        .on_press(StreamingMessage::StreamModeChanged(StreamMode::Multicast))
                        .padding([4, 8])
                        .style(if self.stream_mode == StreamMode::Multicast {
                            iced::theme::Button::Primary
                        } else {
                            iced::theme::Button::Secondary
                        }),
                    "Multicast 239.0.1.64 (requires wired LAN)",
                    tooltip::Position::Bottom,
                )
                .style(iced::theme::Container::Box),
            ]
            .spacing(5),
            Space::with_height(5),
            row![
                text("Port:").size(11),
                tooltip(
                    text_input("11000", &self.listen_port)
                        .on_input(StreamingMessage::PortChanged)
                        .width(Length::Fixed(70.0))
                        .size(11),
                    "Video port (audio uses port+1)",
                    tooltip::Position::Bottom,
                )
                .style(iced::theme::Container::Box),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
        ]
        .spacing(5);

        // Stream controls
        let screenshot_button = if self.is_streaming {
            button(text("Screenshot").size(11))
                .on_press(StreamingMessage::TakeScreenshot)
                .padding([6, 10])
        } else {
            button(text("Screenshot").size(11)).padding([6, 10])
        };

        let stream_controls = column![
            text("Stream Control").size(12),
            row![
                if self.is_streaming {
                    tooltip(
                        button(text("STOP").size(11))
                            .on_press(StreamingMessage::StopStream)
                            .padding([6, 14]),
                        "Stop video stream",
                        tooltip::Position::Bottom,
                    )
                    .style(iced::theme::Container::Box)
                } else {
                    tooltip(
                        button(text("START").size(11))
                            .on_press(StreamingMessage::StartStream)
                            .padding([6, 14]),
                        "Start video stream",
                        tooltip::Position::Bottom,
                    )
                    .style(iced::theme::Container::Box)
                },
                tooltip(
                    screenshot_button,
                    if self.is_streaming {
                        "Capture frame to Pictures folder"
                    } else {
                        "Start streaming first"
                    },
                    tooltip::Position::Bottom,
                )
                .style(iced::theme::Container::Box),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
            tooltip(
                checkbox("Audio", self.audio_enabled)
                    .on_toggle(StreamingMessage::AudioToggled)
                    .size(16)
                    .text_size(11),
                "Enable audio streaming (port+1)",
                tooltip::Position::Bottom,
            )
            .style(iced::theme::Container::Box),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Command prompt section
        let command_history_items: Vec<Element<'_, StreamingMessage>> = self
            .command_history
            .iter()
            .rev()
            .take(10)
            .map(|cmd| text(cmd).size(10).into())
            .collect();

        let command_section = column![
            text("COMMAND PROMPT").size(12),
            row![
                text("C64>").size(11),
                text_input("Enter BASIC command...", &self.command_input)
                    .on_input(StreamingMessage::CommandInputChanged)
                    .on_submit(StreamingMessage::SendCommand)
                    .width(Length::Fill)
                    .size(11),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
            button(text("Send").size(11))
                .on_press(StreamingMessage::SendCommand)
                .padding([4, 12])
                .width(Length::Fill),
            scrollable(Column::with_children(command_history_items).spacing(2))
                .height(Length::Fill),
        ]
        .spacing(5);

        // Right panel with all controls
        let right_panel = container(
            column![
                mode_section,
                iced::widget::horizontal_rule(1),
                stream_controls,
                iced::widget::horizontal_rule(1),
                command_section,
            ]
            .spacing(10)
            .padding(10)
            .width(Length::Fixed(220.0)),
        )
        .height(Length::Fill);

        // Main layout: video on left, controls on right
        let main_content = row![
            container(video_display).width(Length::Fill).center_x(),
            iced::widget::vertical_rule(1),
            right_panel,
        ]
        .spacing(10)
        .height(Length::Fill);

        column![
            text("VIC VIDEO STREAM").size(20),
            iced::widget::horizontal_rule(1),
            main_content,
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    pub fn subscription(&self) -> Subscription<StreamingMessage> {
        if self.is_streaming {
            iced::time::every(Duration::from_millis(40)).map(|_| StreamingMessage::FrameUpdate)
        } else {
            Subscription::none()
        }
    }

    fn start_stream(&mut self) {
        if self.is_streaming {
            return;
        }

        let port: u16 = self.listen_port.parse().unwrap_or(11000);
        let mode = self.stream_mode;

        log::info!("Starting video stream... mode={:?}, port={}", mode, port);
        self.stop_signal.store(false, Ordering::Relaxed);

        // Reset packet counter
        if let Ok(mut p) = self.packets_received.lock() {
            *p = 0;
        }

        let frame_buffer = self.frame_buffer.clone();
        let stop_signal = self.stop_signal.clone();
        let packets_counter = self.packets_received.clone();
        self.is_streaming = true;

        let handle = thread::spawn(move || {
            log::info!("Video stream thread started");

            let socket = match mode {
                StreamMode::Unicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => {
                        log::info!("Unicast socket bound to 0.0.0.0:{}", port);
                        s
                    }
                    Err(e) => {
                        log::error!("Failed to bind unicast socket: {}", e);
                        return;
                    }
                },
                StreamMode::Multicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => {
                        let multicast_addr: std::net::Ipv4Addr = "239.0.1.64".parse().unwrap();
                        let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                        if let Err(e) = s.join_multicast_v4(&multicast_addr, &interface) {
                            log::error!("Failed to join multicast group: {}", e);
                            return;
                        }
                        log::info!("Multicast socket joined 239.0.1.64:{}", port);
                        s
                    }
                    Err(e) => {
                        log::error!("Failed to bind multicast socket: {}", e);
                        return;
                    }
                },
            };

            if let Err(e) = socket.set_nonblocking(true) {
                log::error!("Failed to set non-blocking: {}", e);
                return;
            }

            // Buffer for receiving packets
            let mut recv_buf = [0u8; 1024];

            // RGBA frame buffer (384 * 272 * 4 bytes)
            let rgba_size = (VIC_WIDTH * VIC_HEIGHT * 4) as usize;
            let mut rgba_frame: Vec<u8> = vec![0u8; rgba_size];
            let mut first_packet = true;

            // Build color lookup table (2 pixels packed per byte -> 8 bytes RGBA output)
            let mut color_lut: Vec<[u8; 8]> = Vec::with_capacity(256);
            for i in 0..256 {
                let hi = (i >> 4) & 0x0F;
                let lo = i & 0x0F;
                let c_hi = &C64_PALETTE[hi];
                let c_lo = &C64_PALETTE[lo];
                color_lut.push([
                    c_lo[0], c_lo[1], c_lo[2], 255, // LEFT pixel (low nibble)
                    c_hi[0], c_hi[1], c_hi[2], 255, // RIGHT pixel (high nibble)
                ]);
            }

            loop {
                if stop_signal.load(Ordering::Relaxed) {
                    break;
                }

                match socket.recv_from(&mut recv_buf) {
                    Ok((size, _addr)) => {
                        if size < HEADER_SIZE {
                            continue;
                        }

                        // Count packets
                        if let Ok(mut p) = packets_counter.lock() {
                            *p += 1;
                        }

                        // Parse header
                        let line_raw = u16::from_le_bytes([recv_buf[4], recv_buf[5]]);
                        let pixels_in_line =
                            u16::from_le_bytes([recv_buf[6], recv_buf[7]]) as usize;
                        let lines_in_packet = recv_buf[8] as usize;

                        // Log first packet info
                        if first_packet {
                            first_packet = false;
                            log::info!(
                                "First video packet: pixels_in_line={}, lines_in_packet={}, payload_size={}",
                                pixels_in_line,
                                lines_in_packet,
                                size - HEADER_SIZE
                            );
                        }

                        let line_num = (line_raw & 0x7FFF) as usize;
                        let is_frame_end = (line_raw & 0x8000) != 0;

                        let payload = &recv_buf[HEADER_SIZE..size];
                        let bytes_per_line = pixels_in_line / 2;

                        // Process each line in the packet
                        for l in 0..lines_in_packet {
                            let y = line_num + l;
                            if y >= VIC_HEIGHT as usize {
                                continue;
                            }

                            let payload_offset = l * bytes_per_line;
                            let row_offset = y * (VIC_WIDTH as usize) * 4;

                            for x in 0..bytes_per_line {
                                if payload_offset + x >= payload.len() {
                                    break;
                                }
                                let packed_byte = payload[payload_offset + x] as usize;
                                let colors = &color_lut[packed_byte];

                                let pixel_x = x * 2;
                                if pixel_x + 1 < VIC_WIDTH as usize {
                                    let offset = row_offset + pixel_x * 4;
                                    if offset + 7 < rgba_frame.len() {
                                        rgba_frame[offset..offset + 8].copy_from_slice(colors);
                                    }
                                }
                            }
                        }

                        // On frame end, copy to shared buffer
                        if is_frame_end {
                            if let Ok(mut fb) = frame_buffer.lock() {
                                *fb = Some(rgba_frame.clone());
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => {
                        log::debug!("Socket recv error: {}", e);
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }

            // Leave multicast group if needed
            if mode == StreamMode::Multicast {
                let multicast_addr: std::net::Ipv4Addr = "239.0.1.64".parse().unwrap();
                let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                let _ = socket.leave_multicast_v4(&multicast_addr, &interface);
            }

            log::info!("Video stream thread stopped");
        });

        self.stream_handle = Some(handle);

        // Start audio stream if enabled
        if self.audio_enabled {
            self.start_audio_stream(port + AUDIO_PORT_OFFSET, mode);
        }
    }

    fn start_audio_stream(&mut self, port: u16, mode: StreamMode) {
        log::info!("Starting audio stream on port {}", port);

        // Reset audio packet counter
        if let Ok(mut p) = self.audio_packets_received.lock() {
            *p = 0;
        }

        // Create shared audio buffer using f32 for better Mac compatibility
        let audio_buffer: Arc<Mutex<VecDeque<f32>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_BUFFER_SIZE * 2)));
        self.audio_buffer = Some(audio_buffer.clone());

        let consumer_buffer = audio_buffer.clone();
        let producer_buffer = audio_buffer.clone();
        let stop_signal = self.stop_signal.clone();
        let stop_signal_net = self.stop_signal.clone();
        let audio_packets_counter = self.audio_packets_received.clone();

        // Start audio output thread using cpal
        let audio_handle = thread::spawn(move || {
            log::info!("Audio playback thread started");

            let host = cpal::default_host();
            log::info!("Audio host: {}", host.id().name());

            let device = match host.default_output_device() {
                Some(d) => d,
                None => {
                    log::error!("No audio output device found");
                    return;
                }
            };

            let device_name = device.name().unwrap_or_else(|_| "Unknown".to_string());
            log::info!("Using audio device: {}", device_name);

            // Log supported configs for debugging
            match device.supported_output_configs() {
                Ok(configs) => {
                    for config in configs {
                        log::debug!("Supported output config: {:?}", config);
                    }
                }
                Err(e) => {
                    log::warn!("Could not query supported configs: {}", e);
                }
            }

            // Try to get a supported config, preferring f32 format
            let supported_config = match device.supported_output_configs() {
                Ok(configs) => {
                    let configs_vec: Vec<_> = configs.collect();

                    // First try: f32 with matching channels and sample rate
                    configs_vec
                        .iter()
                        .find(|c| {
                            c.channels() == AUDIO_CHANNELS
                                && c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                                && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                                && c.sample_format() == cpal::SampleFormat::F32
                        })
                        .or_else(|| {
                            // Second try: i16 with matching channels and sample rate
                            configs_vec.iter().find(|c| {
                                c.channels() == AUDIO_CHANNELS
                                    && c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                                    && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                                    && c.sample_format() == cpal::SampleFormat::I16
                            })
                        })
                        .or_else(|| {
                            // Third try: any format with matching channels and sample rate
                            configs_vec.iter().find(|c| {
                                c.channels() == AUDIO_CHANNELS
                                    && c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                                    && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                            })
                        })
                        .cloned()
                        .map(|c| c.with_sample_rate(cpal::SampleRate(AUDIO_SAMPLE_RATE)))
                }
                Err(e) => {
                    log::error!("Failed to get supported configs: {}", e);
                    None
                }
            };

            let (stream_config, sample_format) = match supported_config {
                Some(ref c) => {
                    log::info!("Using supported config: {:?}", c);
                    (c.config(), c.sample_format())
                }
                None => {
                    log::warn!("No matching config found, trying default f32 config");
                    (
                        cpal::StreamConfig {
                            channels: AUDIO_CHANNELS,
                            sample_rate: cpal::SampleRate(AUDIO_SAMPLE_RATE),
                            buffer_size: cpal::BufferSize::Default,
                        },
                        cpal::SampleFormat::F32,
                    )
                }
            };

            log::info!(
                "Audio stream config: {} channels, {} Hz, format: {:?}, buffer: {:?}",
                stream_config.channels,
                stream_config.sample_rate.0,
                sample_format,
                stream_config.buffer_size
            );

            // Build stream based on the supported sample format
            let stream: cpal::Stream = match sample_format {
                cpal::SampleFormat::F32 => {
                    let consumer = consumer_buffer;
                    match device.build_output_stream(
                        &stream_config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            if let Ok(mut buf) = consumer.lock() {
                                for sample in data.iter_mut() {
                                    *sample = buf.pop_front().unwrap_or(0.0);
                                }
                            } else {
                                for sample in data.iter_mut() {
                                    *sample = 0.0;
                                }
                            }
                        },
                        |err| log::error!("Audio stream error: {}", err),
                        None,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to build f32 audio stream: {}", e);
                            return;
                        }
                    }
                }
                cpal::SampleFormat::I16 => {
                    let consumer = consumer_buffer;
                    match device.build_output_stream(
                        &stream_config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            if let Ok(mut buf) = consumer.lock() {
                                for sample in data.iter_mut() {
                                    // Convert f32 back to i16
                                    let f = buf.pop_front().unwrap_or(0.0);
                                    *sample = (f * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                }
                            } else {
                                for sample in data.iter_mut() {
                                    *sample = 0;
                                }
                            }
                        },
                        |err| log::error!("Audio stream error: {}", err),
                        None,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to build i16 audio stream: {}", e);
                            return;
                        }
                    }
                }
                cpal::SampleFormat::U16 => {
                    let consumer = consumer_buffer;
                    match device.build_output_stream(
                        &stream_config,
                        move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                            if let Ok(mut buf) = consumer.lock() {
                                for sample in data.iter_mut() {
                                    // Convert f32 (-1.0 to 1.0) to u16 (0 to 65535)
                                    let f = buf.pop_front().unwrap_or(0.0);
                                    *sample = ((f + 1.0) * 32767.5).clamp(0.0, 65535.0) as u16;
                                }
                            } else {
                                for sample in data.iter_mut() {
                                    *sample = 32768; // Silence for unsigned
                                }
                            }
                        },
                        |err| log::error!("Audio stream error: {}", err),
                        None,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to build u16 audio stream: {}", e);
                            return;
                        }
                    }
                }
                _ => {
                    log::error!("Unsupported sample format: {:?}", sample_format);
                    return;
                }
            };

            if let Err(e) = stream.play() {
                log::error!("Failed to start audio playback: {}", e);
                return;
            }

            log::info!("Audio playback started successfully");

            // Keep thread alive while streaming
            while !stop_signal.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }

            // Stream will be dropped here, stopping playback
            drop(stream);
            log::info!("Audio playback thread stopped");
        });

        // Start audio network receiver thread
        let network_handle = thread::spawn(move || {
            log::info!("Audio network thread started on port {}", port);

            let socket = match mode {
                StreamMode::Unicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => {
                        log::info!("Audio unicast socket bound to 0.0.0.0:{}", port);
                        s
                    }
                    Err(e) => {
                        log::error!("Failed to bind audio socket: {}", e);
                        return;
                    }
                },
                StreamMode::Multicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => {
                        let multicast_addr: std::net::Ipv4Addr = "239.0.1.65".parse().unwrap();
                        let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                        if let Err(e) = s.join_multicast_v4(&multicast_addr, &interface) {
                            log::error!("Failed to join audio multicast group: {}", e);
                            return;
                        }
                        log::info!("Audio multicast socket joined 239.0.1.65:{}", port);
                        s
                    }
                    Err(e) => {
                        log::error!("Failed to bind audio multicast socket: {}", e);
                        return;
                    }
                },
            };

            if let Err(e) = socket.set_nonblocking(true) {
                log::error!("Failed to set audio socket non-blocking: {}", e);
                return;
            }

            let mut recv_buf = [0u8; 2048];
            let mut first_packet = true;

            loop {
                if stop_signal_net.load(Ordering::Relaxed) {
                    break;
                }

                match socket.recv_from(&mut recv_buf) {
                    Ok((size, _addr)) => {
                        if size <= AUDIO_HEADER_SIZE {
                            continue;
                        }

                        // Count packets
                        if let Ok(mut p) = audio_packets_counter.lock() {
                            *p += 1;
                        }

                        // Log first packet for debugging
                        if first_packet {
                            first_packet = false;
                            log::info!(
                                "First audio packet: {} bytes (payload: {} bytes, {} samples)",
                                size,
                                size - AUDIO_HEADER_SIZE,
                                (size - AUDIO_HEADER_SIZE) / 2
                            );
                        }

                        // Skip 2-byte sequence header, rest is i16 samples (little-endian)
                        let audio_data = &recv_buf[AUDIO_HEADER_SIZE..size];

                        // Convert bytes to f32 samples (i16 -> f32 normalized to -1.0..1.0)
                        if let Ok(mut buf) = producer_buffer.lock() {
                            for chunk in audio_data.chunks_exact(2) {
                                let sample_i16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                                let sample_f32 = sample_i16 as f32 / 32768.0;

                                // Keep buffer size limited to prevent memory growth
                                if buf.len() < AUDIO_BUFFER_SIZE * 2 {
                                    buf.push_back(sample_f32);
                                }
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => {
                        log::debug!("Audio socket recv error: {}", e);
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }

            if mode == StreamMode::Multicast {
                let multicast_addr: std::net::Ipv4Addr = "239.0.1.65".parse().unwrap();
                let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                let _ = socket.leave_multicast_v4(&multicast_addr, &interface);
            }

            log::info!("Audio network thread stopped");
        });

        self.audio_stream_handle = Some(audio_handle);
        self.audio_network_handle = Some(network_handle);
    }

    fn stop_stream(&mut self) {
        if !self.is_streaming {
            return;
        }

        log::info!("Stopping video and audio streams...");
        self.stop_signal.store(true, Ordering::Relaxed);

        // Stop video thread
        if let Some(handle) = self.stream_handle.take() {
            let _ = handle.join();
        }

        // Stop audio playback thread
        if let Some(handle) = self.audio_stream_handle.take() {
            let _ = handle.join();
        }

        // Stop audio network thread
        if let Some(handle) = self.audio_network_handle.take() {
            let _ = handle.join();
        }

        // Clear audio buffer
        self.audio_buffer = None;

        self.is_streaming = false;
        if let Ok(mut frame) = self.frame_buffer.lock() {
            *frame = None;
        }
        if let Ok(mut img) = self.image_buffer.lock() {
            *img = None;
        }

        log::info!("All streams stopped");
    }
}

impl Drop for VideoStreaming {
    fn drop(&mut self) {
        if self.is_streaming {
            self.stop_stream();
        }
    }
}

// Custom style for black background in fullscreen mode
struct BlackBackground;

impl iced::widget::container::StyleSheet for BlackBackground {
    type Style = iced::Theme;

    fn appearance(&self, _style: &Self::Style) -> iced::widget::container::Appearance {
        iced::widget::container::Appearance {
            background: Some(iced::Background::Color(iced::Color::BLACK)),
            text_color: Some(iced::Color::WHITE),
            ..Default::default()
        }
    }
}

// Decode VIC stream frame to RGBA (used for raw frame data, not packet-based data)
#[allow(dead_code)]
fn decode_vic_frame(raw_data: &[u8]) -> Option<Vec<u8>> {
    let expected_indexed = (VIC_WIDTH * VIC_HEIGHT) as usize;
    let expected_rgb = expected_indexed * 3;
    let expected_rgba = expected_indexed * 4;

    log::debug!("Decoding frame: {} bytes", raw_data.len());

    if raw_data.len() == expected_indexed {
        // Indexed color mode - convert using C64 palette
        let mut rgba = Vec::with_capacity(expected_rgba);
        for &pixel in raw_data {
            let idx = (pixel & 0x0F) as usize;
            let color = &C64_PALETTE[idx];
            rgba.push(color[0]);
            rgba.push(color[1]);
            rgba.push(color[2]);
            rgba.push(255);
        }
        Some(rgba)
    } else if raw_data.len() == expected_rgb {
        // RGB mode - convert to RGBA
        let mut rgba = Vec::with_capacity(expected_rgba);
        for chunk in raw_data.chunks(3) {
            if chunk.len() == 3 {
                rgba.push(chunk[0]);
                rgba.push(chunk[1]);
                rgba.push(chunk[2]);
                rgba.push(255);
            }
        }
        Some(rgba)
    } else if raw_data.len() == expected_rgba {
        // Already RGBA
        Some(raw_data.to_vec())
    } else if raw_data.len() >= expected_indexed {
        // Fallback for unknown format but has enough data - try indexed interpretation
        let mut rgba = Vec::with_capacity(expected_rgba);
        for &pixel in raw_data.iter().take(expected_indexed) {
            let idx = (pixel & 0x0F) as usize;
            let color = &C64_PALETTE[idx];
            rgba.push(color[0]);
            rgba.push(color[1]);
            rgba.push(color[2]);
            rgba.push(255);
        }
        Some(rgba)
    } else {
        log::warn!(
            "Unknown frame format: {} bytes (expected {} or {} or {})",
            raw_data.len(),
            expected_indexed,
            expected_rgb,
            expected_rgba
        );
        None
    }
}

/// Save screenshot from existing RGBA buffer to user's Pictures folder
pub async fn save_screenshot_to_pictures(rgba_data: Vec<u8>) -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    // Get user's Pictures folder, fallback to home directory
    let pictures_dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "Could not find Pictures or Home directory".to_string())?;

    // Create Ultimate64 subfolder
    let screenshot_dir = pictures_dir.join("Ultimate64");
    std::fs::create_dir_all(&screenshot_dir)
        .map_err(|e| format!("Failed to create screenshot directory: {}", e))?;

    let filename = format!("u64_screenshot_{}.png", timestamp);
    let path = screenshot_dir.join(&filename);

    // Create image and save
    let img = image::RgbaImage::from_raw(VIC_WIDTH, VIC_HEIGHT, rgba_data)
        .ok_or_else(|| "Failed to create image from frame data".to_string())?;

    img.save(&path)
        .map_err(|e| format!("Failed to save PNG: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

#[allow(dead_code)]
pub async fn take_screenshot_async(port: u16, mode: StreamMode) -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let filename = format!("screenshot_{}.png", timestamp);
    let path = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join(&filename);

    // Capture a complete frame using proper packet parsing
    let rgba_data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let socket = match mode {
            StreamMode::Unicast => UdpSocket::bind(format!("0.0.0.0:{}", port))
                .map_err(|e| format!("Failed to bind socket: {}", e))?,
            StreamMode::Multicast => {
                let s = UdpSocket::bind(format!("0.0.0.0:{}", port))
                    .map_err(|e| format!("Failed to bind socket: {}", e))?;
                let multicast_addr: std::net::Ipv4Addr = "239.0.1.64".parse().unwrap();
                let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                s.join_multicast_v4(&multicast_addr, &interface)
                    .map_err(|e| format!("Failed to join multicast: {}", e))?;
                s
            }
        };

        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("Failed to set timeout: {}", e))?;

        let mut recv_buf = [0u8; 1024];
        let rgba_size = (VIC_WIDTH * VIC_HEIGHT * 4) as usize;
        let mut rgba_frame: Vec<u8> = vec![0u8; rgba_size];

        // Build color lookup table
        let mut color_lut: Vec<[u8; 8]> = Vec::with_capacity(256);
        for i in 0..256 {
            let hi = (i >> 4) & 0x0F;
            let lo = i & 0x0F;
            let c_hi = &C64_PALETTE[hi];
            let c_lo = &C64_PALETTE[lo];
            color_lut.push([
                c_lo[0], c_lo[1], c_lo[2], 255, // LEFT pixel (low nibble)
                c_hi[0], c_hi[1], c_hi[2], 255, // RIGHT pixel (high nibble)
            ]);
        }

        // Wait for a complete frame
        let start = std::time::Instant::now();
        let mut got_frame = false;

        while !got_frame && start.elapsed() < Duration::from_secs(5) {
            match socket.recv_from(&mut recv_buf) {
                Ok((size, _)) => {
                    if size < HEADER_SIZE {
                        continue;
                    }

                    let line_raw = u16::from_le_bytes([recv_buf[4], recv_buf[5]]);
                    let pixels_in_line = u16::from_le_bytes([recv_buf[6], recv_buf[7]]) as usize;
                    let lines_in_packet = recv_buf[8] as usize;

                    let line_num = (line_raw & 0x7FFF) as usize;
                    let is_frame_end = (line_raw & 0x8000) != 0;

                    let payload = &recv_buf[HEADER_SIZE..size];
                    let half_pixels = pixels_in_line / 2;

                    for l in 0..lines_in_packet {
                        let y = line_num + l;
                        if y >= VIC_HEIGHT as usize {
                            continue;
                        }

                        let line_start = l * half_pixels;
                        let line_end = line_start + half_pixels;

                        if line_end > payload.len() {
                            break;
                        }

                        let row_offset = y * (VIC_WIDTH as usize) * 4;

                        for x in 0..half_pixels {
                            if line_start + x >= payload.len() {
                                break;
                            }
                            let packed_byte = payload[line_start + x] as usize;
                            let colors = &color_lut[packed_byte];

                            let pixel_x = x * 2;
                            if pixel_x + 1 < VIC_WIDTH as usize {
                                let offset = row_offset + pixel_x * 4;
                                if offset + 7 < rgba_frame.len() {
                                    rgba_frame[offset..offset + 8].copy_from_slice(colors);
                                }
                            }
                        }
                    }

                    if is_frame_end {
                        got_frame = true;
                    }
                }
                Err(e) => {
                    return Err(format!("Failed to receive data: {}", e));
                }
            }
        }

        if !got_frame {
            return Err("Timeout waiting for frame".to_string());
        }

        Ok(rgba_frame)
    })
    .await
    .map_err(|e| format!("Task error: {}", e))??;

    // Create image and save
    let img = image::RgbaImage::from_raw(VIC_WIDTH, VIC_HEIGHT, rgba_data)
        .ok_or_else(|| "Failed to create image".to_string())?;

    img.save(&path)
        .map_err(|e| format!("Failed to save PNG: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}
