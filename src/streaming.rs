use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::{
    Command, Element, Length, Subscription,
    event::{self, Event},
    keyboard::{self, Key, Modifiers},
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
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;

// Import keyboard mapping module
use crate::keyboard_map::{KEYBUF_ADDR, KEYBUF_COUNT, KeyboardMapper};

// Video frame dimensions
pub const VIC_WIDTH: u32 = 384;
pub const VIC_HEIGHT: u32 = 272;
// const FRAME_SIZE: usize = (VIC_WIDTH * VIC_HEIGHT) as usize; // 104448 bytes

// Audio constants
const AUDIO_PORT_OFFSET: u16 = 1; // Audio port = video port + 1
const AUDIO_SAMPLE_RATE: u32 = 48000;
const AUDIO_CHANNELS: u16 = 2;
// const AUDIO_SAMPLES_PER_PACKET: usize = 192 * 4; // 768 samples (384 stereo pairs)
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
    Unicast,
    Multicast,
}

impl std::fmt::Display for StreamMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamMode::Unicast => write!(f, "Unicast (Direct IP)"),
            StreamMode::Multicast => write!(f, "Multicast"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScaleMode {
    #[default]
    Nearest,
    Scale2x,
    Scanlines,
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
    ScaleModeChanged(ScaleMode),
    PortChanged(String),
    AudioToggled(bool),
    ToggleFullscreen,
    VideoClicked,
    // Keyboard control messages
    ToggleKeyboard(bool),
    KeyPressed(Key, Modifiers),
    KeyReleased(Key),
    KeySent(Result<(), String>),
}

pub struct VideoStreaming {
    pub is_streaming: bool,
    pub frame_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub image_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub scaled_buffer: Option<Vec<u8>>,
    pub stop_signal: Arc<AtomicBool>,
    stream_handle: Option<thread::JoinHandle<()>>,
    audio_stream_handle: Option<thread::JoinHandle<()>>,
    audio_network_handle: Option<thread::JoinHandle<()>>,
    pub command_input: String,
    pub command_history: Vec<String>,
    pub stream_mode: StreamMode,
    pub scale_mode: ScaleMode,
    pub listen_port: String,
    pub packets_received: Arc<Mutex<u64>>,
    pub audio_packets_received: Arc<Mutex<u64>>,
    pub audio_enabled: bool,
    audio_buffer: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub is_fullscreen: bool,
    last_click_time: Option<std::time::Instant>,
    // Keyboard control
    pub keyboard_enabled: bool,
    keyboard_mapper: KeyboardMapper,
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
            scaled_buffer: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            stream_handle: None,
            audio_stream_handle: None,
            audio_network_handle: None,
            command_input: String::new(),
            command_history: Vec::new(),
            stream_mode: StreamMode::Unicast,
            scale_mode: ScaleMode::Nearest,
            listen_port: "11000".to_string(),
            packets_received: Arc::new(Mutex::new(0)),
            audio_packets_received: Arc::new(Mutex::new(0)),
            audio_enabled: true,
            audio_buffer: None,
            is_fullscreen: false,
            last_click_time: None,
            keyboard_enabled: false,
            keyboard_mapper: KeyboardMapper::new(),
        }
    }

    pub fn update(
        &mut self,
        message: StreamingMessage,
        connection: Option<Arc<TokioMutex<Rest>>>,
    ) -> Command<StreamingMessage> {
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
                if let Ok(frame_guard) = self.frame_buffer.lock() {
                    if let Some(rgba_data) = &*frame_guard {
                        let scaled = match self.scale_mode {
                            ScaleMode::Nearest => rgba_data.clone(),
                            ScaleMode::Scale2x => scale2x(rgba_data, VIC_WIDTH, VIC_HEIGHT),
                            ScaleMode::Scanlines => {
                                apply_scanlines(rgba_data, VIC_WIDTH, VIC_HEIGHT)
                            }
                        };
                        self.scaled_buffer = Some(scaled);

                        if let Ok(mut img_guard) = self.image_buffer.lock() {
                            *img_guard = Some(rgba_data.clone());
                        }
                    }
                }
                Command::none()
            }
            StreamingMessage::TakeScreenshot => {
                if !self.is_streaming {
                    return Command::none();
                }

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
            StreamingMessage::ScreenshotComplete(_result) => Command::none(),
            StreamingMessage::CommandInputChanged(value) => {
                self.command_input = value;
                Command::none()
            }
            StreamingMessage::SendCommand => Command::none(),
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
            StreamingMessage::ScaleModeChanged(mode) => {
                self.scale_mode = mode;
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
            StreamingMessage::ToggleFullscreen => Command::none(),
            StreamingMessage::VideoClicked => {
                let now = std::time::Instant::now();
                if let Some(last_time) = self.last_click_time {
                    if now.duration_since(last_time).as_millis() < 300 {
                        self.last_click_time = None;
                        return Command::perform(async {}, |_| StreamingMessage::ToggleFullscreen);
                    }
                }
                self.last_click_time = Some(now);
                Command::none()
            }
            // Keyboard control messages
            StreamingMessage::ToggleKeyboard(enabled) => {
                self.keyboard_enabled = enabled;
                log::info!(
                    "Keyboard capture: {}",
                    if enabled { "ENABLED" } else { "DISABLED" }
                );

                if !enabled {
                    self.keyboard_mapper.release_all();
                }
                Command::none()
            }
            StreamingMessage::KeyPressed(key, modifiers) => {
                if !self.keyboard_enabled || !self.is_streaming {
                    return Command::none();
                }

                if let Some(petscii) = self.keyboard_mapper.key_down(&key, &modifiers) {
                    log::debug!(
                        "Key press: {:?} -> PETSCII {:#04x} ({})",
                        key,
                        petscii,
                        petscii as char
                    );

                    if let Some(conn) = connection {
                        return Command::perform(
                            async move {
                                let c = conn.lock().await;
                                // Write PETSCII code to keyboard buffer
                                c.write_mem(KEYBUF_ADDR, &[petscii])
                                    .map_err(|e| format!("Buffer write failed: {}", e))?;
                                // Set buffer count to 1
                                c.write_mem(KEYBUF_COUNT, &[1])
                                    .map_err(|e| format!("Count write failed: {}", e))?;
                                Ok(())
                            },
                            StreamingMessage::KeySent,
                        );
                    }
                }
                Command::none()
            }
            StreamingMessage::KeyReleased(key) => {
                if !self.keyboard_enabled || !self.is_streaming {
                    return Command::none();
                }

                // For keyboard buffer approach, we don't need to do anything on release
                // The character was already sent to the buffer on key press
                self.keyboard_mapper.key_up(&key);
                Command::none()
            }
            StreamingMessage::KeySent(result) => {
                if let Err(e) = result {
                    log::error!("Failed to send key to C64: {}", e);
                }
                Command::none()
            }
        }
    }

    pub fn view_fullscreen(&self) -> Element<'_, StreamingMessage> {
        let (img_width, img_height) = match self.scale_mode {
            ScaleMode::Nearest => (VIC_WIDTH, VIC_HEIGHT),
            ScaleMode::Scale2x => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
            ScaleMode::Scanlines => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
        };

        let video_content: Element<'_, StreamingMessage> = if self.is_streaming {
            let frame_data = if self.scale_mode != ScaleMode::Nearest {
                self.scaled_buffer.clone()
            } else {
                self.image_buffer.lock().ok().and_then(|g| g.clone())
            };

            if let Some(rgba_data) = frame_data {
                let handle =
                    iced::widget::image::Handle::from_pixels(img_width, img_height, rgba_data);
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
            text("Stream not active - press ESC to exit")
                .size(20)
                .style(iced::theme::Text::Color(iced::Color::WHITE))
                .into()
        };

        let keyboard_status = if self.keyboard_enabled {
            text("⌨ KEYBOARD ACTIVE")
                .size(12)
                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.3, 1.0, 0.3,
                )))
        } else {
            text("").size(12)
        };

        let exit_hint = container(
            row![
                button(text("Exit Fullscreen (ESC or double-click)").size(12))
                    .on_press(StreamingMessage::ToggleFullscreen)
                    .padding([6, 12]),
                Space::with_width(20),
                button(
                    text(if self.keyboard_enabled {
                        "⌨ Keyboard ON"
                    } else {
                        "⌨ Keyboard OFF"
                    })
                    .size(12)
                )
                .on_press(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled))
                .padding([6, 12])
                .style(if self.keyboard_enabled {
                    iced::theme::Button::Primary
                } else {
                    iced::theme::Button::Secondary
                }),
                Space::with_width(20),
                keyboard_status,
            ]
            .spacing(10)
            .align_items(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .center_x()
        .padding(10);

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
        let video_packets = self.packets_received.lock().map(|p| *p).unwrap_or(0);
        let audio_packets = self.audio_packets_received.lock().map(|p| *p).unwrap_or(0);

        let (display_width, display_height) = match self.scale_mode {
            ScaleMode::Nearest => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
            ScaleMode::Scale2x => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
            ScaleMode::Scanlines => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
        };

        let (img_width, img_height) = match self.scale_mode {
            ScaleMode::Nearest => (VIC_WIDTH, VIC_HEIGHT),
            ScaleMode::Scale2x => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
            ScaleMode::Scanlines => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
        };

        // Video display
        let video_display: Element<'_, StreamingMessage> = if self.is_streaming {
            let frame_data = if self.scale_mode != ScaleMode::Nearest {
                self.scaled_buffer.clone()
            } else {
                self.image_buffer.lock().ok().and_then(|g| g.clone())
            };

            if let Some(rgba_data) = frame_data {
                let handle =
                    iced::widget::image::Handle::from_pixels(img_width, img_height, rgba_data);
                let video_image = mouse_area(
                    iced_image(handle)
                        .width(Length::Fixed(display_width as f32))
                        .height(Length::Fixed(display_height as f32))
                        .content_fit(iced::ContentFit::Fill),
                )
                .on_press(StreamingMessage::VideoClicked);

                let scale_label = match self.scale_mode {
                    ScaleMode::Nearest => "Nearest",
                    ScaleMode::Scale2x => "Scale2x",
                    ScaleMode::Scanlines => "Scanlines",
                };

                let keyboard_indicator = if self.keyboard_enabled {
                    text("⌨ KEYBOARD ACTIVE - Type to control C64!")
                        .size(11)
                        .style(iced::theme::Text::Color(iced::Color::from_rgb(
                            0.3, 0.9, 0.3,
                        )))
                } else {
                    text("Double-click for fullscreen").size(10)
                };

                container(
                    column![
                        video_image,
                        text(format!(
                            "{}x{} [{}] | Video: {} | Audio: {}",
                            VIC_WIDTH, VIC_HEIGHT, scale_label, video_packets, audio_packets
                        ))
                        .size(10),
                        keyboard_indicator,
                    ]
                    .spacing(5)
                    .align_items(iced::Alignment::Center),
                )
                .padding(10)
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
                    "Direct UDP connection (requires Ethernet)",
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
                text_input("11000", &self.listen_port)
                    .on_input(StreamingMessage::PortChanged)
                    .width(Length::Fixed(70.0))
                    .size(11),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
        ]
        .spacing(5);

        // Scale mode
        let scale_section = column![
            text("Video Scale").size(12),
            row![
                button(text("Nearest").size(10))
                    .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Nearest))
                    .padding([4, 6])
                    .style(if self.scale_mode == ScaleMode::Nearest {
                        iced::theme::Button::Primary
                    } else {
                        iced::theme::Button::Secondary
                    }),
                button(text("Scale2x").size(10))
                    .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Scale2x))
                    .padding([4, 6])
                    .style(if self.scale_mode == ScaleMode::Scale2x {
                        iced::theme::Button::Primary
                    } else {
                        iced::theme::Button::Secondary
                    }),
                button(text("Scanlines").size(10))
                    .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Scanlines))
                    .padding([4, 6])
                    .style(if self.scale_mode == ScaleMode::Scanlines {
                        iced::theme::Button::Primary
                    } else {
                        iced::theme::Button::Secondary
                    }),
            ]
            .spacing(3),
        ]
        .spacing(5);

        // Stream controls
        let stream_controls = column![
            text("Stream Control").size(12),
            row![
                if self.is_streaming {
                    button(text("STOP").size(11))
                        .on_press(StreamingMessage::StopStream)
                        .padding([6, 14])
                } else {
                    button(text("START").size(11))
                        .on_press(StreamingMessage::StartStream)
                        .padding([6, 14])
                },
                if self.is_streaming {
                    button(text("Screenshot").size(11))
                        .on_press(StreamingMessage::TakeScreenshot)
                        .padding([6, 10])
                } else {
                    button(text("Screenshot").size(11)).padding([6, 10])
                },
            ]
            .spacing(5),
            checkbox("Audio", self.audio_enabled)
                .on_toggle(StreamingMessage::AudioToggled)
                .size(16)
                .text_size(11),
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Keyboard control section
        let keyboard_section = column![
            text("Remote Keyboard").size(12),
            button(
                text(if self.keyboard_enabled {
                    "⌨ ENABLED"
                } else {
                    "⌨ Disabled"
                })
                .size(11)
            )
            .on_press(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled))
            .padding([6, 12])
            .width(Length::Fill)
            .style(if self.keyboard_enabled {
                iced::theme::Button::Primary
            } else {
                iced::theme::Button::Secondary
            }),
            if self.keyboard_enabled {
                text("Arrow keys, letters, F1-F8\nESC=RUN/STOP, Tab=CTRL\nAlt=Commodore key")
                    .size(9)
                    .style(iced::theme::Text::Color(iced::Color::from_rgb(
                        0.6, 0.6, 0.6,
                    )))
            } else {
                text("Click to enable keyboard\ncontrol for C64")
                    .size(9)
                    .style(iced::theme::Text::Color(iced::Color::from_rgb(
                        0.5, 0.5, 0.5,
                    )))
            },
        ]
        .spacing(5)
        .align_items(iced::Alignment::Center);

        // Command prompt
        let command_history_items: Vec<Element<'_, StreamingMessage>> = self
            .command_history
            .iter()
            .rev()
            .take(10)
            .map(|cmd| text(cmd).size(10).into())
            .collect();

        let command_section = column![
            text("BASIC PROMPT").size(12),
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

        // Right panel
        let right_panel = container(
            column![
                mode_section,
                iced::widget::horizontal_rule(1),
                scale_section,
                iced::widget::horizontal_rule(1),
                stream_controls,
                iced::widget::horizontal_rule(1),
                keyboard_section,
                iced::widget::horizontal_rule(1),
                command_section,
            ]
            .spacing(10)
            .padding(10)
            .width(Length::Fixed(220.0)),
        )
        .height(Length::Fill);

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
        let mut subscriptions = Vec::new();

        if self.is_streaming {
            subscriptions.push(
                iced::time::every(Duration::from_millis(40)).map(|_| StreamingMessage::FrameUpdate),
            );
        }

        // Keyboard events when enabled and streaming
        if self.keyboard_enabled && self.is_streaming {
            subscriptions.push(event::listen_with(|event, _status| match event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    Some(StreamingMessage::KeyPressed(key, modifiers))
                }
                Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                    Some(StreamingMessage::KeyReleased(key))
                }
                _ => None,
            }));
        }

        Subscription::batch(subscriptions)
    }

    fn start_stream(&mut self) {
        if self.is_streaming {
            return;
        }

        let port: u16 = self.listen_port.parse().unwrap_or(11000);
        let mode = self.stream_mode;

        log::info!("Starting video stream... mode={:?}, port={}", mode, port);
        self.stop_signal.store(false, Ordering::Relaxed);

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

            let mut recv_buf = [0u8; 1024];
            let rgba_size = (VIC_WIDTH * VIC_HEIGHT * 4) as usize;
            let mut rgba_frame: Vec<u8> = vec![0u8; rgba_size];
            let mut first_packet = true;

            // Build color lookup table
            let mut color_lut: Vec<[u8; 8]> = Vec::with_capacity(256);
            for i in 0..256 {
                let hi = (i >> 4) & 0x0F;
                let lo = i & 0x0F;
                let c_hi = &C64_PALETTE[hi];
                let c_lo = &C64_PALETTE[lo];
                color_lut.push([
                    c_lo[0], c_lo[1], c_lo[2], 255, c_hi[0], c_hi[1], c_hi[2], 255,
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

                        if let Ok(mut p) = packets_counter.lock() {
                            *p += 1;
                        }

                        let line_raw = u16::from_le_bytes([recv_buf[4], recv_buf[5]]);
                        let pixels_in_line =
                            u16::from_le_bytes([recv_buf[6], recv_buf[7]]) as usize;
                        let lines_in_packet = recv_buf[8] as usize;

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

            if mode == StreamMode::Multicast {
                let multicast_addr: std::net::Ipv4Addr = "239.0.1.64".parse().unwrap();
                let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                let _ = socket.leave_multicast_v4(&multicast_addr, &interface);
            }

            log::info!("Video stream thread stopped");
        });

        self.stream_handle = Some(handle);

        if self.audio_enabled {
            self.start_audio_stream(port + AUDIO_PORT_OFFSET, mode);
        }
    }

    fn start_audio_stream(&mut self, port: u16, mode: StreamMode) {
        log::info!("Starting audio stream on port {}", port);

        if let Ok(mut p) = self.audio_packets_received.lock() {
            *p = 0;
        }

        let audio_buffer: Arc<Mutex<VecDeque<f32>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_BUFFER_SIZE * 2)));
        self.audio_buffer = Some(audio_buffer.clone());

        let consumer_buffer = audio_buffer.clone();
        let producer_buffer = audio_buffer.clone();
        let stop_signal = self.stop_signal.clone();
        let stop_signal_net = self.stop_signal.clone();
        let audio_packets_counter = self.audio_packets_received.clone();

        // Audio playback thread
        let audio_handle = thread::spawn(move || {
            log::info!("Audio playback thread started");

            let host = cpal::default_host();
            let device = match host.default_output_device() {
                Some(d) => d,
                None => {
                    log::error!("No audio output device found");
                    return;
                }
            };

            let supported_config = match device.supported_output_configs() {
                Ok(configs) => {
                    let configs_vec: Vec<_> = configs.collect();
                    configs_vec
                        .iter()
                        .find(|c| {
                            c.channels() == AUDIO_CHANNELS
                                && c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                                && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                                && c.sample_format() == cpal::SampleFormat::F32
                        })
                        .or_else(|| {
                            configs_vec.iter().find(|c| {
                                c.channels() == AUDIO_CHANNELS
                                    && c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                                    && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                            })
                        })
                        .cloned()
                        .map(|c| c.with_sample_rate(cpal::SampleRate(AUDIO_SAMPLE_RATE)))
                }
                Err(_) => None,
            };

            let (stream_config, sample_format) = match supported_config {
                Some(ref c) => (c.config(), c.sample_format()),
                None => (
                    cpal::StreamConfig {
                        channels: AUDIO_CHANNELS,
                        sample_rate: cpal::SampleRate(AUDIO_SAMPLE_RATE),
                        buffer_size: cpal::BufferSize::Default,
                    },
                    cpal::SampleFormat::F32,
                ),
            };

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
                            }
                        },
                        |err| log::error!("Audio stream error: {}", err),
                        None,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to build audio stream: {}", e);
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
                                    let f = buf.pop_front().unwrap_or(0.0);
                                    *sample = (f * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                }
                            }
                        },
                        |err| log::error!("Audio stream error: {}", err),
                        None,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to build audio stream: {}", e);
                            return;
                        }
                    }
                }
                _ => {
                    log::error!("Unsupported sample format");
                    return;
                }
            };

            let _ = stream.play();
            while !stop_signal.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
            drop(stream);
        });

        // Audio network thread
        let network_handle = thread::spawn(move || {
            let socket = match mode {
                StreamMode::Unicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("Failed to bind audio socket: {}", e);
                        return;
                    }
                },
                StreamMode::Multicast => match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                    Ok(s) => {
                        let multicast_addr: std::net::Ipv4Addr = "239.0.1.65".parse().unwrap();
                        let interface: std::net::Ipv4Addr = "0.0.0.0".parse().unwrap();
                        let _ = s.join_multicast_v4(&multicast_addr, &interface);
                        s
                    }
                    Err(e) => {
                        log::error!("Failed to bind audio socket: {}", e);
                        return;
                    }
                },
            };

            let _ = socket.set_nonblocking(true);
            let mut recv_buf = [0u8; 2048];

            loop {
                if stop_signal_net.load(Ordering::Relaxed) {
                    break;
                }

                match socket.recv_from(&mut recv_buf) {
                    Ok((size, _)) => {
                        if size <= AUDIO_HEADER_SIZE {
                            continue;
                        }

                        if let Ok(mut p) = audio_packets_counter.lock() {
                            *p += 1;
                        }

                        let audio_data = &recv_buf[AUDIO_HEADER_SIZE..size];
                        if let Ok(mut buf) = producer_buffer.lock() {
                            for chunk in audio_data.chunks_exact(2) {
                                let sample_i16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                                let sample_f32 = sample_i16 as f32 / 32768.0;
                                if buf.len() < AUDIO_BUFFER_SIZE * 2 {
                                    buf.push_back(sample_f32);
                                }
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        });

        self.audio_stream_handle = Some(audio_handle);
        self.audio_network_handle = Some(network_handle);
    }

    fn stop_stream(&mut self) {
        if !self.is_streaming {
            return;
        }

        log::info!("Stopping streams...");
        self.stop_signal.store(true, Ordering::Relaxed);
        self.keyboard_enabled = false;
        self.keyboard_mapper.release_all();

        if let Some(handle) = self.stream_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.audio_stream_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.audio_network_handle.take() {
            let _ = handle.join();
        }

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

pub async fn save_screenshot_to_pictures(rgba_data: Vec<u8>) -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let pictures_dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "Could not find Pictures directory".to_string())?;

    let screenshot_dir = pictures_dir.join("Ultimate64");
    std::fs::create_dir_all(&screenshot_dir)
        .map_err(|e| format!("Failed to create directory: {}", e))?;

    let filename = format!("u64_screenshot_{}.png", timestamp);
    let path = screenshot_dir.join(&filename);

    let img = image::RgbaImage::from_raw(VIC_WIDTH, VIC_HEIGHT, rgba_data)
        .ok_or_else(|| "Failed to create image".to_string())?;

    img.save(&path)
        .map_err(|e| format!("Failed to save PNG: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

fn scale2x(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let out_w = w * 2;
    let out_h = h * 2;
    let mut output = vec![0u8; out_w * out_h * 4];

    for y in 0..h {
        for x in 0..w {
            let p = get_pixel(input, w, h, x, y);
            let a = get_pixel(input, w, h, x, y.saturating_sub(1));
            let b = get_pixel(input, w, h, x.saturating_add(1).min(w - 1), y);
            let c = get_pixel(input, w, h, x.saturating_sub(1), y);
            let d = get_pixel(input, w, h, x, y.saturating_add(1).min(h - 1));

            let p0 = if colors_equal(&a, &c) && !colors_equal(&a, &b) && !colors_equal(&c, &d) {
                a
            } else {
                p
            };
            let p1 = if colors_equal(&a, &b) && !colors_equal(&a, &c) && !colors_equal(&b, &d) {
                b
            } else {
                p
            };
            let p2 = if colors_equal(&c, &d) && !colors_equal(&a, &c) && !colors_equal(&b, &d) {
                c
            } else {
                p
            };
            let p3 = if colors_equal(&b, &d) && !colors_equal(&a, &b) && !colors_equal(&c, &d) {
                d
            } else {
                p
            };

            let out_x = x * 2;
            let out_y = y * 2;
            set_pixel(&mut output, out_w, out_x, out_y, &p0);
            set_pixel(&mut output, out_w, out_x + 1, out_y, &p1);
            set_pixel(&mut output, out_w, out_x, out_y + 1, &p2);
            set_pixel(&mut output, out_w, out_x + 1, out_y + 1, &p3);
        }
    }
    output
}

fn apply_scanlines(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let out_w = w * 2;
    let out_h = h * 2;
    let mut output = vec![0u8; out_w * out_h * 4];
    let scanline_brightness: f32 = 0.55;

    for y in 0..h {
        for x in 0..w {
            let pixel = get_pixel(input, w, h, x, y);
            let dark_pixel = [
                (pixel[0] as f32 * scanline_brightness) as u8,
                (pixel[1] as f32 * scanline_brightness) as u8,
                (pixel[2] as f32 * scanline_brightness) as u8,
                pixel[3],
            ];

            let out_x = x * 2;
            let out_y = y * 2;
            set_pixel(&mut output, out_w, out_x, out_y, &pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y, &pixel);
            set_pixel(&mut output, out_w, out_x, out_y + 1, &dark_pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y + 1, &dark_pixel);
        }
    }
    output
}

#[inline]
fn get_pixel(data: &[u8], width: usize, height: usize, x: usize, y: usize) -> [u8; 4] {
    if x >= width || y >= height {
        return [0, 0, 0, 255];
    }
    let idx = (y * width + x) * 4;
    if idx + 3 < data.len() {
        [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]
    } else {
        [0, 0, 0, 255]
    }
}

#[inline]
fn set_pixel(data: &mut [u8], width: usize, x: usize, y: usize, pixel: &[u8; 4]) {
    let idx = (y * width + x) * 4;
    if idx + 3 < data.len() {
        data[idx] = pixel[0];
        data[idx + 1] = pixel[1];
        data[idx + 2] = pixel[2];
        data[idx + 3] = pixel[3];
    }
}

#[inline]
fn colors_equal(a: &[u8; 4], b: &[u8; 4]) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}
