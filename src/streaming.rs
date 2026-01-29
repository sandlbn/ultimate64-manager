use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::widget::image::FilterMethod;
use iced::{
    Element, Length, Subscription, Task,
    event::{self, Event},
    keyboard::{self, Key, Modifiers},
    widget::{
        Column, Space, button, checkbox, column, container, image as iced_image, mouse_area, row,
        rule, scrollable, text, text_input, tooltip,
    },
};

use crate::settings::StreamControlMethod;
use crate::stream_control::{get_local_ip, send_stop_command, send_stream_command};
use crate::video_scaling::{
    C64_PALETTE, apply_crt_effect, apply_scanlines, integer_scale, scale2x,
};

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;
use ultimate64::petscii::Petscii;

// Video frame dimensions
pub const VIC_WIDTH: u32 = 384;
pub const VIC_HEIGHT: u32 = 272;
// const FRAME_SIZE: usize = (VIC_WIDTH * VIC_HEIGHT) as usize; // 104448 bytes

// Audio constants
const AUDIO_PORT_OFFSET: u16 = 1; // Audio port = video port + 1
const AUDIO_SAMPLE_RATE: u32 = 48000; // Output sample rate (what cpal uses)
// const AUDIO_SAMPLES_PER_PACKET: usize = 192 * 4; // 768 samples (384 stereo pairs)
const AUDIO_HEADER_SIZE: usize = 2; // Just sequence number
const AUDIO_BUFFER_SIZE: usize = AUDIO_SAMPLE_RATE as usize; // ~1 second buffer

// Derived from the C64's clock frequencies
const AUDIO_SAMPLE_RATE_PAL: f64 = 47982.8869047619;
#[allow(dead_code)]
const AUDIO_SAMPLE_RATE_NTSC: f64 = 47940.3408482143;

// Jitter buffer settings
const JITTER_MIN_FRAMES: usize = 4800; // 100ms
const JITTER_TARGET_FRAMES: usize = 9600; // 200ms

const JITTER_BUFFER_MIN_SAMPLES: usize = JITTER_MIN_FRAMES * 2; // interleaved stereo f32
const JITTER_BUFFER_TARGET_SAMPLES: usize = JITTER_TARGET_FRAMES * 2;
const JITTER_BUFFER_MAX_SAMPLES: usize = AUDIO_SAMPLE_RATE as usize * 2; // 1s stereo f32

// Ultimate64 video packet header (12 bytes)
// struct {
//     uint16_t seq;           // 0-1
//     uint16_t frame;         // 2-3
//     uint16_t line;          // 4-5 (MSB = frame sync flag)
//     uint16_t pixelsInLine;  // 6-7
//     uint8_t linesInPacket;  // 8
//     uint8_t bpp;            // 9
//     uint16_t encoding;      // 10-11
//     char payload[768];      // 12+
// }
const HEADER_SIZE: usize = 12;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScaleMode {
    #[default]
    Int2x, // Integer 2x scale (sharp pixels)
    Int3x, // Integer 3x scale

    Scale2x,   // EPX/Scale2x smoothing
    Scanlines, // CRT scanline effect
    CRT,       // Shadow mask + scanlines
}

#[derive(Debug, Clone)]
pub enum StreamingMessage {
    StartStream,
    StopStream,
    FrameUpdate,
    TakeScreenshot,
    ScreenshotComplete(Result<String, String>),
    OpenScreenshot(String),
    CommandInputChanged(String),
    SendCommand,
    CommandSent(Result<String, String>),
    StreamModeChanged(StreamMode),
    ScaleModeChanged(ScaleMode),
    PortChanged(String),
    AudioToggled(bool),
    ToggleFullscreen,
    VideoClicked, // For double-click detection
    // Keyboard control messages
    ToggleKeyboard(bool),        // Enable/disable keyboard capture
    KeyPressed(Key, Modifiers),  // Key press event with modifiers
    KeyReleased(Key),            // Key release event
    KeySent(Result<(), String>), // Result of sending key to C64
    // Ultimate64 host configuration
    UltimateHostChanged(String), // Set the host for stream control
}

/// Simple linear resampler for converting Ultimate64's ~47983 Hz to 48000 Hz
/// This prevents audio drift that would otherwise cause buffer underrun/overflow
struct AudioResampler {
    step: f64, // input_rate / output_rate  (NOT the other way)
    pos: f64,
    last_left: f32,
    last_right: f32,
}

impl AudioResampler {
    fn new(input_rate: f64, output_rate: f64) -> Self {
        Self {
            step: input_rate / output_rate,
            pos: 0.0,
            last_left: 0.0,
            last_right: 0.0,
        }
    }

    fn process_stereo(&mut self, input: &[f32], output: &mut VecDeque<f32>) {
        for chunk in input.chunks_exact(2) {
            let left = chunk[0];
            let right = chunk[1];

            // generate 0/1/2 output frames per input frame depending on step/pos
            while self.pos <= 1.0 {
                let t = self.pos as f32;
                output.push_back(self.last_left + (left - self.last_left) * t);
                output.push_back(self.last_right + (right - self.last_right) * t);
                self.pos += self.step;
            }

            self.pos -= 1.0;
            self.last_left = left;
            self.last_right = right;
        }
    }
}

/// Audio buffer state for jitter buffer management
struct AudioBufferState {
    /// Sample buffer (interleaved stereo f32)
    samples: VecDeque<f32>,
    /// Whether initial buffering is complete
    buffering_complete: bool,
    /// Last sequence number seen (for gap detection)
    last_seq: Option<u16>,
    /// Count of detected packet gaps
    packet_gaps: u64,
    /// Count of dropped samples due to overflow
    samples_dropped: u64,
}

impl AudioBufferState {
    fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(AUDIO_BUFFER_SIZE * 2),
            buffering_complete: false,
            last_seq: None,
            packet_gaps: 0,
            samples_dropped: 0,
        }
    }
}
impl ScaleMode {
    fn to_u8(self) -> u8 {
        match self {
            ScaleMode::Int2x => 0,
            ScaleMode::Int3x => 1,
            ScaleMode::Scale2x => 2,
            ScaleMode::Scanlines => 3,
            ScaleMode::CRT => 4,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => ScaleMode::Int2x,
            1 => ScaleMode::Int3x,
            2 => ScaleMode::Scale2x,
            3 => ScaleMode::Scanlines,
            4 => ScaleMode::CRT,
            _ => ScaleMode::Int2x,
        }
    }
}

/// Combined frame data and dimensions - must be updated atomically to prevent display corruption
#[derive(Clone)]
pub struct ScaledFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct VideoStreaming {
    pub is_streaming: bool,
    pub frame_buffer: Arc<Mutex<Option<ScaledFrame>>>, // Scaled RGBA + dimensions for display
    pub current_handle: Option<iced::widget::image::Handle>, // Cached image handle for display
    pub current_dimensions: (u32, u32),                // Dimensions of current handle
    pub stop_signal: Arc<AtomicBool>,
    stream_handle: Option<thread::JoinHandle<()>>, // Video receive thread
    audio_stream_handle: Option<thread::JoinHandle<()>>, // Audio playback thread
    audio_network_handle: Option<thread::JoinHandle<()>>, // Audio receive thread
    pub command_input: String,
    pub command_history: Vec<String>,
    pub stream_mode: StreamMode,
    pub scale_mode: ScaleMode,
    pub listen_port: String,
    pub packets_received: Arc<Mutex<u64>>, // Video packet counter
    pub audio_packets_received: Arc<Mutex<u64>>, // Audio packet counter
    pub audio_enabled: bool,
    audio_buffer: Option<Arc<Mutex<AudioBufferState>>>, // Shared audio sample buffer with jitter management
    pub is_fullscreen: bool,
    last_click_time: Option<std::time::Instant>, // For double-click detection
    // Keyboard control
    pub keyboard_enabled: bool, // Whether keyboard capture is active
    last_key_time: Option<std::time::Instant>, // Rate limiting for keyboard
    // Ultimate64 host for stream control (binary protocol on port 64)
    pub ultimate_host: Option<String>,
    // API password for REST API fallback
    pub api_password: Option<String>,
    scale_mode_shared: Arc<std::sync::atomic::AtomicU8>, // For thread to read
    pub stream_control_method: StreamControlMethod, // Stream control method for communicating with Ultimate64
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
            current_handle: None,
            current_dimensions: (VIC_WIDTH * 2, VIC_HEIGHT * 2),
            stop_signal: Arc::new(AtomicBool::new(false)),
            stream_handle: None,
            audio_stream_handle: None,
            audio_network_handle: None,
            command_input: String::new(),
            command_history: Vec::new(),
            stream_mode: StreamMode::Unicast,
            scale_mode: ScaleMode::Int2x,
            listen_port: "11000".to_string(),
            packets_received: Arc::new(Mutex::new(0)),
            audio_packets_received: Arc::new(Mutex::new(0)),
            audio_enabled: true,
            audio_buffer: None,
            is_fullscreen: false,
            last_click_time: None,
            keyboard_enabled: false,
            last_key_time: None,
            ultimate_host: None,
            api_password: None,
            scale_mode_shared: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            stream_control_method: StreamControlMethod::default(),
        }
    }

    /// Set the Ultimate64 host for stream control
    pub fn set_ultimate_host(&mut self, host: Option<String>) {
        self.ultimate_host = host;
    }

    /// Set the API password for REST API stream control
    pub fn set_api_password(&mut self, password: Option<String>) {
        self.api_password = password;
    }

    /// Set the stream control method
    pub fn set_stream_control_method(&mut self, method: StreamControlMethod) {
        self.stream_control_method = method;
    }
    pub fn update(
        &mut self,
        message: StreamingMessage,
        connection: Option<Arc<TokioMutex<Rest>>>,
    ) -> Task<StreamingMessage> {
        match message {
            StreamingMessage::StartStream => {
                self.start_stream();
                Task::none()
            }
            StreamingMessage::StopStream => {
                self.stop_stream();
                Task::none()
            }
            StreamingMessage::FrameUpdate => {
                // Create the image Handle here, only once per frame update
                // This avoids creating new Handles on every view() call
                if let Ok(fb) = self.frame_buffer.lock() {
                    if let Some(frame) = &*fb {
                        self.current_handle = Some(iced::widget::image::Handle::from_rgba(
                            frame.width,
                            frame.height,
                            frame.data.clone(),
                        ));
                        self.current_dimensions = (frame.width, frame.height);
                    }
                }
                Task::none()
            }
            StreamingMessage::TakeScreenshot => {
                // Take screenshot from the existing scaled frame buffer
                if !self.is_streaming {
                    return Task::none();
                }

                // Get current scaled frame from buffer (includes dimensions)
                let frame_data = if let Ok(fb_guard) = self.frame_buffer.lock() {
                    fb_guard.clone()
                } else {
                    None
                };

                if let Some(frame) = frame_data {
                    Task::perform(
                        save_screenshot_to_pictures(frame.data, frame.width, frame.height),
                        StreamingMessage::ScreenshotComplete,
                    )
                } else {
                    Task::perform(
                        async { Err("No frame available".to_string()) },
                        StreamingMessage::ScreenshotComplete,
                    )
                }
            }
            StreamingMessage::ScreenshotComplete(_result) => {
                // Handled by main app for user message display
                Task::none()
            }
            StreamingMessage::OpenScreenshot(path) => {
                // Open the screenshot file in default viewer
                if let Err(e) = open::that(&path) {
                    log::warn!("Failed to open screenshot: {}", e);
                }
                Task::none()
            }
            StreamingMessage::CommandInputChanged(value) => {
                self.command_input = value;
                Task::none()
            }
            StreamingMessage::SendCommand => {
                // Handled by main.rs which has access to the Rest connection
                Task::none()
            }
            StreamingMessage::CommandSent(result) => {
                match result {
                    Ok(msg) => self.command_history.push(msg),
                    Err(e) => self.command_history.push(format!("Error: {}", e)),
                }
                Task::none()
            }
            StreamingMessage::StreamModeChanged(mode) => {
                self.stream_mode = mode;
                Task::none()
            }
            StreamingMessage::ScaleModeChanged(mode) => {
                self.scale_mode = mode;
                self.scale_mode_shared
                    .store(mode.to_u8(), Ordering::Relaxed);
                Task::none()
            }
            StreamingMessage::PortChanged(port) => {
                self.listen_port = port;
                Task::none()
            }
            StreamingMessage::AudioToggled(enabled) => {
                self.audio_enabled = enabled;
                Task::none()
            }
            StreamingMessage::ToggleFullscreen => Task::none(),
            StreamingMessage::VideoClicked => {
                // Check for double-click (within 300ms)
                let now = std::time::Instant::now();
                if let Some(last_time) = self.last_click_time {
                    if now.duration_since(last_time).as_millis() < 300 {
                        // Double-click detected
                        self.last_click_time = None;
                        return Task::perform(async {}, |_| StreamingMessage::ToggleFullscreen);
                    }
                }
                self.last_click_time = Some(now);
                Task::none()
            }

            // ==================== Keyboard Control Messages ====================
            StreamingMessage::ToggleKeyboard(enabled) => {
                self.keyboard_enabled = enabled;
                log::info!(
                    "Keyboard capture: {}",
                    if enabled { "ENABLED" } else { "DISABLED" }
                );
                Task::none()
            }

            StreamingMessage::KeyPressed(key, modifiers) => {
                if !self.keyboard_enabled || !self.is_streaming {
                    return Task::none();
                }

                // Rate limit: minimum 30ms between key sends to avoid flooding API
                const MIN_KEY_INTERVAL_MS: u64 = 30;
                let now = std::time::Instant::now();
                if let Some(last) = self.last_key_time {
                    if now.duration_since(last).as_millis() < MIN_KEY_INTERVAL_MS as u128 {
                        // Too fast, skip this key event
                        return Task::none();
                    }
                }
                self.last_key_time = Some(now);

                // Convert key to PETSCII
                let petscii: Option<u8> = match &key {
                    Key::Character(c) => {
                        let is_shift = modifiers.shift();

                        // Handle shifted characters - US keyboard layout
                        let code = if is_shift {
                            match c.as_str() {
                                "'" => Some(34),   // Shift+' = "
                                ";" => Some(58),   // Shift+; = :
                                "," => Some(60),   // Shift+, =
                                "." => Some(62),   // Shift+. = >
                                "/" => Some(63),   // Shift+/ = ?
                                "1" => Some(33),   // Shift+1 = !
                                "2" => Some(64),   // Shift+2 = @
                                "3" => Some(35),   // Shift+3 = #
                                "4" => Some(36),   // Shift+4 = $
                                "5" => Some(37),   // Shift+5 = %
                                "6" => Some(94),   // Shift+6 = ^ (up arrow on C64)
                                "7" => Some(38),   // Shift+7 = &
                                "8" => Some(42),   // Shift+8 = *
                                "9" => Some(40),   // Shift+9 = (
                                "0" => Some(41),   // Shift+0 = )
                                "-" => Some(95),   // Shift+- = _ (underscore)
                                "=" => Some(43),   // Shift+= = +
                                "[" => Some(123),  // Shift+[ = { (not on C64, but try)
                                "]" => Some(125),  // Shift+] = } (not on C64, but try)
                                "\\" => Some(124), // Shift+\ = |
                                "`" => Some(126),  // Shift+` = ~
                                _ => None,         // Fall through to normal handling
                            }
                        } else {
                            None // Not shifted, use normal handling
                        };

                        // If shift mapping found, use it; otherwise try normal mapping
                        code.or_else(|| {
                            // Try explicit mapping for direct characters
                            match c.as_str() {
                                "\"" => Some(34), // Double quote (if OS sends it directly)
                                ":" => Some(58),  // Colon
                                ";" => Some(59),  // Semicolon
                                "<" => Some(60),  // Less than
                                ">" => Some(62),  // Greater than
                                "=" => Some(61),  // Equals
                                "+" => Some(43),  // Plus
                                "-" => Some(45),  // Minus
                                "*" => Some(42),  // Asterisk
                                "/" => Some(47),  // Slash
                                "?" => Some(63),  // Question mark
                                "@" => Some(64),  // At sign
                                "!" => Some(33),  // Exclamation
                                "#" => Some(35),  // Hash
                                "$" => Some(36),  // Dollar
                                "%" => Some(37),  // Percent
                                "&" => Some(38),  // Ampersand
                                "'" => Some(39),  // Single quote
                                "(" => Some(40),  // Open paren
                                ")" => Some(41),  // Close paren
                                "," => Some(44),  // Comma
                                "." => Some(46),  // Period
                                "[" => Some(91),  // Open bracket
                                "]" => Some(93),  // Close bracket
                                "^" => Some(94),  // Caret
                                "_" => Some(164), // Underscore
                                " " => Some(32),  // Space
                                _ => {
                                    // Fall back to Petscii::from_str_lossy for letters/numbers
                                    let petscii_bytes = Petscii::from_str_lossy(c);
                                    if petscii_bytes.len() > 0 {
                                        let code = petscii_bytes[0];
                                        // 0x7F is the "unknown character" replacement
                                        if code != 0x7F {
                                            Some(code)
                                        } else {
                                            log::debug!("KEYBOARD: Unknown char '{}' -> 0x7F", c);
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                }
                            }
                        })
                    }
                    Key::Named(named) => {
                        // Special keys need manual PETSCII mapping
                        use iced::keyboard::key::Named;
                        match named {
                            Named::Enter => Some(13),      // RETURN
                            Named::Space => Some(32),      // SPACE
                            Named::Backspace => Some(20),  // DEL (C64 delete)
                            Named::Delete => Some(20),     // DEL
                            Named::Home => Some(19),       // HOME
                            Named::End => Some(147),       // CLR (Shift+HOME)
                            Named::Escape => Some(3),      // RUN/STOP
                            Named::ArrowUp => Some(145),   // CRSR UP
                            Named::ArrowDown => Some(17),  // CRSR DOWN
                            Named::ArrowLeft => Some(157), // CRSR LEFT
                            Named::ArrowRight => Some(29), // CRSR RIGHT
                            Named::F1 => Some(133),        // F1
                            Named::F2 => Some(137),        // F2
                            Named::F3 => Some(134),        // F3
                            Named::F4 => Some(138),        // F4
                            Named::F5 => Some(135),        // F5
                            Named::F6 => Some(139),        // F6
                            Named::F7 => Some(136),        // F7
                            Named::F8 => Some(140),        // F8
                            _ => None,
                        }
                    }
                    _ => None,
                };

                if let Some(code) = petscii {
                    log::debug!("KEYBOARD: {:?} -> PETSCII {} (0x{:02X})", key, code, code);

                    if let Some(conn) = connection {
                        return Task::perform(
                            async move {
                                // Use timeout to prevent hangs
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_millis(500),
                                    tokio::task::spawn_blocking(move || {
                                        let c = conn.blocking_lock();

                                        // Match type_text behavior from ultimate64 library:
                                        // 1. Clear LSTX ($C5) and NDX ($C6) - last key and buffer count
                                        c.write_mem(0x00C5, &[0, 0])
                                            .map_err(|e| format!("Clear failed: {}", e))?;

                                        // 2. Write PETSCII code to keyboard buffer at $0277
                                        c.write_mem(0x0277, &[code])
                                            .map_err(|e| format!("Buffer write failed: {}", e))?;

                                        // 3. Set buffer count to 1 at $C6 to trigger processing
                                        c.write_mem(0x00C6, &[1])
                                            .map_err(|e| format!("Count write failed: {}", e))?;

                                        Ok(())
                                    }),
                                )
                                .await;

                                match result {
                                    Ok(Ok(r)) => r,
                                    Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                    Err(_) => Err("Keyboard write timed out".to_string()),
                                }
                            },
                            StreamingMessage::KeySent,
                        );
                    } else {
                        log::warn!("KEYBOARD: No connection available!");
                    }
                } else {
                    log::trace!("KEYBOARD: Key {:?} not mapped", key);
                }
                Task::none()
            }

            StreamingMessage::KeyReleased(_key) => {
                // For keyboard buffer approach, we don't need to do anything on release
                // The character was already sent to the buffer on key press
                Task::none()
            }

            StreamingMessage::KeySent(result) => {
                if let Err(e) = result {
                    log::error!("Failed to send key to C64: {}", e);
                }
                Task::none()
            }

            StreamingMessage::UltimateHostChanged(host) => {
                if host.is_empty() {
                    self.ultimate_host = None;
                } else {
                    self.ultimate_host = Some(host);
                }
                Task::none()
            }
        }
    }

    /// Fullscreen view - video fills the entire available space with black letterboxing
    pub fn view_fullscreen(&self) -> Element<'_, StreamingMessage> {
        let video_content: Element<'_, StreamingMessage> = if self.is_streaming {
            // Use cached handle - created once in FrameUpdate, not on every view()
            if let Some(ref handle) = self.current_handle {
                let w = self.current_dimensions.0 as f32;
                let h = self.current_dimensions.1 as f32;

                let video_image = mouse_area(
                    iced_image(handle.clone())
                        .width(Length::Fixed(w))
                        .height(Length::Fixed(h))
                        .filter_method(FilterMethod::Nearest),
                )
                .on_press(StreamingMessage::VideoClicked);

                container(video_image)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
            } else {
                text("Waiting for frames...")
                    .size(20)
                    .color(iced::Color::WHITE)
                    .into()
            }
        } else {
            text("Stream not active - press ESC to exit")
                .size(20)
                .color(iced::Color::WHITE)
                .into()
        };

        // Exit hint at the top with keyboard toggle
        let keyboard_btn = button(
            text(if self.keyboard_enabled {
                "⌨ Enabled"
            } else {
                "⌨ Disabled"
            })
            .size(12),
        )
        .on_press(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled))
        .padding([6, 12])
        .style(if self.keyboard_enabled {
            button::primary
        } else {
            button::secondary
        });

        let exit_hint = container(
            row![
                button(text("Exit Fullscreen (ESC or double-click)").size(12))
                    .on_press(StreamingMessage::ToggleFullscreen)
                    .padding([6, 12]),
                keyboard_btn,
            ]
            .spacing(10),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(10);

        // Black background container with centered video
        container(column![
            exit_hint,
            container(video_content)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::BLACK)),
            text_color: Some(iced::Color::WHITE),
            ..Default::default()
        })
        .into()
    }

    pub fn view(&self) -> Element<'_, StreamingMessage> {
        // Video packets info
        let video_packets = self.packets_received.lock().map(|p| *p).unwrap_or(0);
        let audio_packets = self.audio_packets_received.lock().map(|p| *p).unwrap_or(0);

        // === LEFT SIDE: Video display (fluid scaling) ===
        let video_display: Element<'_, StreamingMessage> = if self.is_streaming {
            // Use cached handle - created once in FrameUpdate, not on every view()
            if let Some(ref handle) = self.current_handle {
                let w = self.current_dimensions.0 as f32;
                let h = self.current_dimensions.1 as f32;

                let video_image = mouse_area(
                    iced_image(handle.clone())
                        .width(Length::Fixed(w))
                        .height(Length::Fixed(h))
                        .filter_method(FilterMethod::Nearest),
                )
                .on_press(StreamingMessage::VideoClicked);

                let scale_label = match self.scale_mode {
                    ScaleMode::Int2x => "2x",
                    ScaleMode::Int3x => "3x",
                    ScaleMode::Scale2x => "Smooth",
                    ScaleMode::Scanlines => "Scanlines",
                    ScaleMode::CRT => "CRT",
                };

                column![
                    container(video_image)
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                    text(format!(
                        "{}x{} [{}] | Video: {} | Audio: {} | Double-click for fullscreen",
                        VIC_WIDTH, VIC_HEIGHT, scale_label, video_packets, audio_packets
                    ))
                    .size(10),
                ]
                .spacing(5)
                .align_x(iced::Alignment::Center)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            } else {
                // Image not decoded yet, show raw frame info
                if let Ok(frame_guard) = self.frame_buffer.lock() {
                    if let Some(frame) = &*frame_guard {
                        container(
                            column![
                                text("RECEIVING FRAMES").size(16),
                                text(format!(
                                    "{} bytes ({}x{})",
                                    frame.data.len(),
                                    frame.width,
                                    frame.height
                                ))
                                .size(12),
                                text(format!(
                                    "Video: {} | Audio: {}",
                                    video_packets, audio_packets
                                ))
                                .size(12),
                            ]
                            .spacing(5)
                            .align_x(iced::Alignment::Center),
                        )
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
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
                            .align_x(iced::Alignment::Center),
                        )
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .into()
                    }
                } else {
                    container(text("Waiting for frames...").size(14))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .into()
                }
            }
        } else {
            let status_info = match self.stream_mode {
                StreamMode::Unicast => {
                    format!("Unicast mode: Will send to Ultimate64 at port 64 to start stream",)
                }
                StreamMode::Multicast => {
                    "Multicast mode: 239.0.1.64 (requires wired LAN)".to_string()
                }
            };

            container(
                column![
                    text("VIDEO STREAM INACTIVE").size(16),
                    Space::new().height(10),
                    text(status_info.clone()).size(11),
                    Space::new().height(5),
                    text("Click START to begin streaming").size(11),
                ]
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
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
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "Direct UDP connection (requires Ethernet, WiFi not supported)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Multicast").size(11))
                        .on_press(StreamingMessage::StreamModeChanged(StreamMode::Multicast))
                        .padding([4, 8])
                        .style(if self.stream_mode == StreamMode::Multicast {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "Multicast 239.0.1.64 (requires wired LAN)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(5),
            Space::new().height(5),
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
                .style(container::bordered_box),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(5);

        // Scale mode selection
        let scale_section = column![
            text("Video Scale").size(12),
            row![
                tooltip(
                    button(text("2x").size(10))
                        .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Int2x))
                        .padding([4, 6])
                        .style(if self.scale_mode == ScaleMode::Int2x {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "Integer 2x (sharp pixels)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("3x").size(10))
                        .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Int3x))
                        .padding([4, 6])
                        .style(if self.scale_mode == ScaleMode::Int3x {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "Integer 3x (sharp pixels)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Smooth").size(10))
                        .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Scale2x))
                        .padding([4, 6])
                        .style(if self.scale_mode == ScaleMode::Scale2x {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "Scale2x edge smoothing",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(3),
            row![
                tooltip(
                    button(text("Scanlines").size(10))
                        .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::Scanlines))
                        .padding([4, 6])
                        .style(if self.scale_mode == ScaleMode::Scanlines {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "CRT scanlines",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("CRT").size(10))
                        .on_press(StreamingMessage::ScaleModeChanged(ScaleMode::CRT))
                        .padding([4, 6])
                        .style(if self.scale_mode == ScaleMode::CRT {
                            button::primary
                        } else {
                            button::secondary
                        }),
                    "CRT shadow mask + scanlines",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
            .spacing(3),
        ]
        .spacing(5); // Stream controls with keyboard toggle
        let screenshot_button = if self.is_streaming {
            button(text("Screenshot").size(11))
                .on_press(StreamingMessage::TakeScreenshot)
                .padding([6, 10])
        } else {
            button(text("Screenshot").size(11)).padding([6, 10])
        };

        // Keyboard toggle button
        let keyboard_button = if self.is_streaming {
            tooltip(
                button(
                    text(if self.keyboard_enabled {
                        "⌨ Enabled"
                    } else {
                        "⌨ Disabled"
                    })
                    .size(11),
                )
                .on_press(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled))
                .padding([6, 10])
                .style(if self.keyboard_enabled {
                    button::primary
                } else {
                    button::secondary
                }),
                "Enable keyboard input to C64 (type in video window)",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box)
        } else {
            tooltip(
                button(text("⌨ Disabled").size(11)).padding([6, 10]),
                "Start streaming first",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box)
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
                    .style(container::bordered_box)
                } else {
                    tooltip(
                        button(text("START").size(11))
                            .on_press(StreamingMessage::StartStream)
                            .padding([6, 14]),
                        "Start video stream",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box)
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
                .style(container::bordered_box),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),
            row![
                tooltip(
                    checkbox(self.audio_enabled)
                        .label("Audio")
                        .on_toggle(StreamingMessage::AudioToggled)
                        .size(16)
                        .text_size(11),
                    "Enable audio streaming (port+1)",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
                keyboard_button,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(5)
        .align_x(iced::Alignment::Center);

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
            .align_y(iced::Alignment::Center),
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
                rule::horizontal(1),
                scale_section,
                rule::horizontal(1),
                stream_controls,
                rule::horizontal(1),
                command_section,
            ]
            .spacing(10)
            .padding(10)
            .width(Length::Fixed(220.0)),
        )
        .height(Length::Fill);

        // Main layout: video on left, controls on right
        let main_content = row![
            container(video_display)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            rule::vertical(1),
            right_panel,
        ]
        .spacing(10)
        .height(Length::Fill);

        column![
            text("VIC VIDEO STREAM").size(20),
            rule::horizontal(1),
            main_content,
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    pub fn subscription(&self) -> Subscription<StreamingMessage> {
        let mut subscriptions = Vec::new();

        // Frame update subscription (~25 fps)
        if self.is_streaming {
            subscriptions.push(
                iced::time::every(Duration::from_millis(40)).map(|_| StreamingMessage::FrameUpdate),
            );
        }

        // Keyboard events subscription - only when enabled and streaming
        if self.keyboard_enabled && self.is_streaming {
            subscriptions.push(event::listen_with(|event, _status, _id| match event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    Some(StreamingMessage::KeyPressed(key, modifiers))
                }
                Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) => {
                    Some(StreamingMessage::KeyReleased(key))
                }
                _ => None,
            }));
        }

        if subscriptions.is_empty() {
            Subscription::none()
        } else {
            Subscription::batch(subscriptions)
        }
    }

    fn start_stream(&mut self) {
        if self.is_streaming {
            return;
        }

        let port: u16 = self.listen_port.parse().unwrap_or(11000);
        let mode = self.stream_mode;

        // Clone what we need BEFORE the closure
        let frame_buffer = self.frame_buffer.clone();
        let stop_signal = self.stop_signal.clone();
        let packets_counter = self.packets_received.clone();
        let scale_mode_shared = self.scale_mode_shared.clone();

        // Sync current scale mode to shared atomic
        self.scale_mode_shared
            .store(self.scale_mode.to_u8(), Ordering::Relaxed);

        log::info!("Starting video stream... mode={:?}, port={}", mode, port);
        self.stop_signal.store(false, Ordering::Relaxed);

        // Reset packet counter
        if let Ok(mut p) = self.packets_received.lock() {
            *p = 0;
        }

        // 1. FIRST: Bind UDP socket BEFORE sending control commands
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

        // 2. Small delay to ensure socket is fully ready
        std::thread::sleep(std::time::Duration::from_millis(100));

        // 3. NOW send control commands to Ultimate64 via binary protocol (port 64)
        if let Some(ultimate_ip) = &self.ultimate_host {
            if let Some(my_ip) = get_local_ip() {
                log::info!(
                    "Sending stream start commands to {} (my IP: {})",
                    ultimate_ip,
                    my_ip
                );

                let password = self.api_password.clone();

                let method = self.stream_control_method;

                let _ = send_stop_command(ultimate_ip, 0x20, password.as_deref(), method);
                let _ = send_stop_command(ultimate_ip, 0x21, password.as_deref(), method);

                if let Err(e) = send_stream_command(
                    ultimate_ip,
                    &my_ip,
                    port,
                    0x20,
                    password.as_deref(),
                    method,
                ) {
                    log::error!("Failed to send video start command: {}", e);
                }

                let audio_port = port + 1;
                if let Err(e) = send_stream_command(
                    ultimate_ip,
                    &my_ip,
                    audio_port,
                    0x21,
                    password.as_deref(),
                    method,
                ) {
                    log::error!("Failed to send audio start command: {}", e);
                }
            } else {
                log::error!("Could not detect local IP address");
            }
        } else {
            log::warn!("No Ultimate64 host configured, skipping stream control commands");
        }

        self.is_streaming = true;

        // 4. Spawn thread - do scaling here
        let handle = thread::spawn(move || {
            log::info!("Video stream thread started");

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

                        // Frame complete - apply scaling and store
                        if is_frame_end {
                            // Clone the frame BEFORE scaling - this prevents new packets
                            // from corrupting the frame while we're scaling it
                            let frame_snapshot = rgba_frame.clone();

                            // Get current scale mode and apply scaling to the snapshot
                            let current_mode =
                                ScaleMode::from_u8(scale_mode_shared.load(Ordering::Relaxed));

                            let (scaled, dims) = match current_mode {
                                ScaleMode::Int2x => (
                                    integer_scale(&frame_snapshot, VIC_WIDTH, VIC_HEIGHT, 2),
                                    (VIC_WIDTH * 2, VIC_HEIGHT * 2),
                                ),
                                ScaleMode::Int3x => (
                                    // WORKAROUND: Use 2x to avoid strobing with large textures
                                    integer_scale(&frame_snapshot, VIC_WIDTH, VIC_HEIGHT, 2),
                                    (VIC_WIDTH * 2, VIC_HEIGHT * 2),
                                ),
                                ScaleMode::Scale2x => (
                                    scale2x(&frame_snapshot, VIC_WIDTH, VIC_HEIGHT),
                                    (VIC_WIDTH * 2, VIC_HEIGHT * 2),
                                ),
                                ScaleMode::Scanlines => (
                                    apply_scanlines(&frame_snapshot, VIC_WIDTH, VIC_HEIGHT),
                                    (VIC_WIDTH * 2, VIC_HEIGHT * 2),
                                ),
                                ScaleMode::CRT => (
                                    apply_crt_effect(&frame_snapshot, VIC_WIDTH, VIC_HEIGHT),
                                    (VIC_WIDTH * 2, VIC_HEIGHT * 2),
                                ),
                            };

                            // Store scaled frame with dimensions atomically
                            // This prevents display corruption from dimension/data mismatch
                            if let Ok(mut fb) = frame_buffer.lock() {
                                *fb = Some(ScaledFrame {
                                    data: scaled,
                                    width: dims.0,
                                    height: dims.1,
                                });
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

        // Reset audio packet counter
        if let Ok(mut p) = self.audio_packets_received.lock() {
            *p = 0;
        }

        // Create shared audio buffer with jitter buffer state
        let audio_buffer: Arc<Mutex<AudioBufferState>> =
            Arc::new(Mutex::new(AudioBufferState::new()));
        self.audio_buffer = Some(audio_buffer.clone());

        let consumer_buffer = audio_buffer.clone(); // For audio playback thread
        let producer_buffer = audio_buffer.clone(); // For network receive thread
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

            // Try to get a supported config, preferring f32 format and stereo
            // 1. Stereo (2ch) with f32 format ->
            // 2. Stereo (2ch) with any format ->
            // 3. Multi-channel (4-8ch) with f32 - upmix stereo to front L/R ->
            // 4. Multi-channel with any format ->
            let supported_config = match device.supported_output_configs() {
                Ok(configs) => {
                    let configs_vec: Vec<_> = configs.collect();

                    // Helper to check sample rate compatibility
                    let rate_ok = |c: &&cpal::SupportedStreamConfigRange| {
                        c.min_sample_rate().0 <= AUDIO_SAMPLE_RATE
                            && c.max_sample_rate().0 >= AUDIO_SAMPLE_RATE
                    };

                    // First try: stereo (2ch) with f32
                    configs_vec
                        .iter()
                        .find(|c| {
                            c.channels() == 2
                                && rate_ok(c)
                                && c.sample_format() == cpal::SampleFormat::F32
                        })
                        .or_else(|| {
                            // Second try: stereo with i16
                            configs_vec.iter().find(|c| {
                                c.channels() == 2
                                    && rate_ok(c)
                                    && c.sample_format() == cpal::SampleFormat::I16
                            })
                        })
                        .or_else(|| {
                            // Third try: stereo with any format
                            configs_vec.iter().find(|c| c.channels() == 2 && rate_ok(c))
                        })
                        .or_else(|| {
                            // Fourth try: multi-channel (4-8ch) with f32 - we'll upmix
                            configs_vec.iter().find(|c| {
                                c.channels() >= 4
                                    && c.channels() <= 8
                                    && rate_ok(c)
                                    && c.sample_format() == cpal::SampleFormat::F32
                            })
                        })
                        .or_else(|| {
                            // Fifth try: multi-channel with i16
                            configs_vec.iter().find(|c| {
                                c.channels() >= 4
                                    && c.channels() <= 8
                                    && rate_ok(c)
                                    && c.sample_format() == cpal::SampleFormat::I16
                            })
                        })
                        .or_else(|| {
                            // Sixth try: multi-channel with any format
                            configs_vec
                                .iter()
                                .find(|c| c.channels() >= 4 && c.channels() <= 8 && rate_ok(c))
                        })
                        .or_else(|| {
                            // Last resort: any config that supports our sample rate
                            configs_vec.iter().find(|c| rate_ok(c))
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
                    log::error!("No compatible audio config found - audio will not work");
                    return;
                }
            };

            // Track output channel count for upmixing stereo -> N channels
            let output_channels = stream_config.channels as usize;

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
                            if let Ok(mut state) = consumer.lock() {
                                // Jitter buffer: wait until we have enough samples before starting playback
                                if !state.buffering_complete {
                                    if state.samples.len() >= JITTER_BUFFER_MIN_SAMPLES {
                                        state.buffering_complete = true;
                                        log::info!(
                                            "Audio jitter buffer filled ({} samples), starting playback",
                                            state.samples.len()
                                        );
                                    } else {
                                        // Still buffering, output silence
                                        for sample in data.iter_mut() {
                                            *sample = 0.0;
                                        }
                                        return;
                                    }
                                }

                                // Handle multi-channel output by upmixing stereo
                                // Stereo source goes to channels 0 (L) and 1 (R), rest get silence
                                if output_channels == 2 {
                                    // Standard stereo - consume samples directly
                                    for sample in data.iter_mut() {
                                        *sample = state.samples.pop_front().unwrap_or(0.0);
                                    }
                                } else {
                                    // Multi-channel: upmix stereo to front L/R only
                                    // Standard channel layout: 0=FL, 1=FR, 2=FC, 3=LFE, 4=RL, 5=RR, ...
                                    for frame in data.chunks_mut(output_channels) {
                                        let left = state.samples.pop_front().unwrap_or(0.0);
                                        let right = state.samples.pop_front().unwrap_or(0.0);
                                        // Front Left and Front Right get the stereo signal
                                        if frame.len() > 0 { frame[0] = left; }
                                        if frame.len() > 1 { frame[1] = right; }
                                        // All other channels get silence
                                        for ch in frame.iter_mut().skip(2) {
                                            *ch = 0.0;
                                        }
                                    }
                                }
                                // Log warning if buffer is getting low (potential underrun)
                                if state.samples.len() < JITTER_BUFFER_MIN_SAMPLES / 2 {
                                    log::trace!(
                                        "Audio buffer low: {} samples remaining",
                                        state.samples.len()
                                    );
                                }
                            } else {
                                data.fill(0.0);
                                return
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
                            if let Ok(mut state) = consumer.lock() {
                                if !state.buffering_complete {
                                    if state.samples.len() >= JITTER_BUFFER_MIN_SAMPLES {
                                        state.buffering_complete = true;
                                        log::info!(
                                            "Audio jitter buffer filled ({} samples), starting playback",
                                            state.samples.len()
                                        );
                                    } else {
                                        for sample in data.iter_mut() {
                                            *sample = 0;
                                        }
                                        return;
                                    }
                                }

                                // Handle multi-channel output
                                if output_channels == 2 {
                                    for sample in data.iter_mut() {
                                        let f = state.samples.pop_front().unwrap_or(0.0);
                                        *sample = (f * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                    }
                                } else {
                                    // Multi-channel: upmix stereo to front L/R only
                                    for frame in data.chunks_mut(output_channels) {
                                        let left = state.samples.pop_front().unwrap_or(0.0);
                                        let right = state.samples.pop_front().unwrap_or(0.0);

                                        if frame.len() > 0 {
                                            frame[0] = (left * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                        }
                                        if frame.len() > 1 {
                                            frame[1] = (right * 32767.0).clamp(-32768.0, 32767.0) as i16;
                                        }
                                        for ch in frame.iter_mut().skip(2) {
                                            *ch = 0;
                                        }
                                    }
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
                            if let Ok(mut state) = consumer.lock() {
                                if !state.buffering_complete {
                                    if state.samples.len() >= JITTER_BUFFER_MIN_SAMPLES {
                                        state.buffering_complete = true;
                                        log::info!(
                                            "Audio jitter buffer filled ({} samples), starting playback",
                                            state.samples.len()
                                        );
                                    } else {
                                        for sample in data.iter_mut() {
                                            *sample = 32768;
                                        }
                                        return;
                                    }
                                }

                                // Handle multi-channel output
                                if output_channels == 2 {
                                    for sample in data.iter_mut() {
                                        let f = state.samples.pop_front().unwrap_or(0.0);
                                        *sample = ((f + 1.0) * 32767.5).clamp(0.0, 65535.0) as u16;
                                    }
                                } else {
                                    // Multi-channel: upmix stereo to front L/R only
                                    for frame in data.chunks_mut(output_channels) {
                                        let left = state.samples.pop_front().unwrap_or(0.0);
                                        let right = state.samples.pop_front().unwrap_or(0.0);

                                        if frame.len() > 0 {
                                            frame[0] = ((left + 1.0) * 32767.5).clamp(0.0, 65535.0) as u16;
                                        }
                                        if frame.len() > 1 {
                                            frame[1] = ((right + 1.0) * 32767.5).clamp(0.0, 65535.0) as u16;
                                        }
                                        // 32768 is silence for u16 audio
                                        for ch in frame.iter_mut().skip(2) {
                                            *ch = 32768;
                                        }
                                    }
                                }
                            } else {
                                for sample in data.iter_mut() {
                                    *sample = 32768;
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

            // Create resampler for converting Ultimate64's ~47983 Hz to 48000 Hz
            let mut resampler =
                AudioResampler::new(AUDIO_SAMPLE_RATE_PAL, AUDIO_SAMPLE_RATE as f64);

            // Temporary buffer for converted samples before resampling
            let mut temp_samples: Vec<f32> = Vec::with_capacity(1024);

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

                        // Parse sequence number for gap detection
                        let packet_seq = u16::from_le_bytes([recv_buf[0], recv_buf[1]]);

                        // Log first packet for debugging
                        if first_packet {
                            first_packet = false;
                            log::info!(
                                "First audio packet: {} bytes (payload: {} bytes, {} samples), seq={}",
                                size,
                                size - AUDIO_HEADER_SIZE,
                                (size - AUDIO_HEADER_SIZE) / 2,
                                packet_seq
                            );
                        }

                        // Skip 2-byte sequence header, rest is i16 samples (little-endian)
                        let audio_data = &recv_buf[AUDIO_HEADER_SIZE..size];

                        // Convert bytes to f32 samples (i16 -> f32 normalized to -1.0..1.0)
                        temp_samples.clear();
                        for chunk in audio_data.chunks_exact(2) {
                            let sample_i16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                            let sample_f32 = sample_i16 as f32 / 32768.0;
                            temp_samples.push(sample_f32);
                        }

                        // Add resampled audio to buffer with jitter buffer management
                        if let Ok(mut state) = producer_buffer.lock() {
                            // Check for packet sequence gaps (packet loss detection)
                            if let Some(last) = state.last_seq {
                                let expected = last.wrapping_add(1);
                                if packet_seq != expected {
                                    // Calculate gap size (handling wraparound)
                                    let gap = if packet_seq > expected {
                                        packet_seq - expected
                                    } else {
                                        // Wraparound case
                                        (0xFFFF - expected) + packet_seq + 1
                                    };
                                    state.packet_gaps += 1;
                                    log::debug!(
                                        "Audio packet gap: expected seq {}, got {} (gap: {}, total gaps: {})",
                                        expected,
                                        packet_seq,
                                        gap,
                                        state.packet_gaps
                                    );
                                }
                            }
                            state.last_seq = Some(packet_seq);

                            // Resample from Ultimate64's ~47983 Hz to 48000 Hz
                            // This prevents long-term drift that would cause buffer underrun/overflow
                            resampler.process_stereo(&temp_samples, &mut state.samples);

                            // Buffer overflow protection: drop oldest samples to stay in sync
                            // This is better than dropping newest because it keeps audio in sync with video
                            if state.samples.len() > JITTER_BUFFER_MAX_SAMPLES {
                                let to_drop = state
                                    .samples
                                    .len()
                                    .saturating_sub(JITTER_BUFFER_TARGET_SAMPLES);
                                for _ in 0..to_drop {
                                    state.samples.pop_front();
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

            // Log final statistics
            if let Ok(state) = producer_buffer.lock() {
                log::info!(
                    "Audio network thread stopped (packet gaps: {}, samples dropped: {})",
                    state.packet_gaps,
                    state.samples_dropped
                );
            } else {
                log::info!("Audio network thread stopped");
            }
        });

        self.audio_stream_handle = Some(audio_handle);
        self.audio_network_handle = Some(network_handle);
    }

    fn stop_stream(&mut self) {
        if !self.is_streaming {
            return;
        }

        log::info!("Stopping video and audio streams...");

        // Set stop signal first so threads start exiting
        self.stop_signal.store(true, Ordering::Relaxed);
        self.keyboard_enabled = false;

        // Send stop commands to Ultimate64 (with timeout to prevent hang)
        if let Some(ultimate_ip) = &self.ultimate_host {
            log::info!("Sending stream stop commands to {}", ultimate_ip);
            let ip = ultimate_ip.clone();
            let password = self.api_password.clone();

            // Run in thread with timeout
            let (tx, rx) = std::sync::mpsc::channel();
            let method = self.stream_control_method;
            std::thread::spawn(move || {
                let _ = send_stop_command(&ip, 0x20, password.as_deref(), method); // Stop video
                let _ = send_stop_command(&ip, 0x21, password.as_deref(), method); // Stop audio
                let _ = tx.send(());
            });

            // Wait max 500ms for stop commands
            if rx.recv_timeout(Duration::from_millis(500)).is_err() {
                log::warn!("Stop commands timed out - device may be offline");
            }
        }

        // Stop video thread with timeout
        if let Some(handle) = self.stream_handle.take() {
            let start = std::time::Instant::now();
            while !handle.is_finished() && start.elapsed() < Duration::from_millis(500) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                log::warn!("Video thread did not stop in time");
            }
        }

        // Stop audio playback thread with timeout
        if let Some(handle) = self.audio_stream_handle.take() {
            let start = std::time::Instant::now();
            while !handle.is_finished() && start.elapsed() < Duration::from_millis(500) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                log::warn!("Audio playback thread did not stop in time");
            }
        }

        // Stop audio network thread with timeout
        if let Some(handle) = self.audio_network_handle.take() {
            let start = std::time::Instant::now();
            while !handle.is_finished() && start.elapsed() < Duration::from_millis(500) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                log::warn!("Audio network thread did not stop in time");
            }
        }

        // Clear audio buffer
        self.audio_buffer = None;

        self.is_streaming = false;

        // Clear frame buffers and handle
        self.current_handle = None;
        if let Ok(mut frame) = self.frame_buffer.lock() {
            *frame = None;
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

/// Save screenshot from existing RGBA buffer to user's Pictures folder
pub async fn save_screenshot_to_pictures(
    rgba_data: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<String, String> {
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

    // Create image and save using provided dimensions
    let img = image::RgbaImage::from_raw(width, height, rgba_data)
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
