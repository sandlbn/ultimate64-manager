use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iced::widget::image::FilterMethod;
use iced::{
    event::{self, Event},
    keyboard::{self, Key, Modifiers},
    widget::{
        button, checkbox, column, container, image as iced_image, mouse_area, responsive, row,
        rule, stack, text, text_input, tooltip, Space,
    },
    Element, Length, Subscription, Task,
};

use crate::net_utils::get_local_ip;
use crate::settings::StreamControlMethod;
use crate::stream_control::{send_stop_command, send_stream_command};
use crate::video_scaling::C64_PALETTE;

use crate::remote_device::RemoteDevice;
use std::collections::VecDeque;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use ultimate64::petscii::Petscii;

/// Multicast group the device sends the VIC video stream to.
const MULTICAST_VIDEO: Ipv4Addr = Ipv4Addr::new(239, 0, 1, 64);
/// Multicast group the device sends the audio stream to.
const MULTICAST_AUDIO: Ipv4Addr = Ipv4Addr::new(239, 0, 1, 65);
/// Local interface to bind multicast joins to (0.0.0.0 = any).
const MULTICAST_IFACE: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

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
    /// Pixel-perfect integer scaling: the frame is shown at the largest whole
    /// multiple that fits, centered, with a letterbox border. No pixel effect.
    #[default]
    PixelPerfect,
    Scale2x,   // EPX/Scale2x smoothing
    Scanlines, // CRT scanline effect
    CRT,       // Shadow mask + scanlines + curvature
    Glow,      // Phosphor bloom — bright pixels bleed light (GPU shader only)
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
    /// Switch between the GPU shader renderer (true) and the compatibility image
    /// renderer (false).
    GpuShaderToggled(bool),
    /// Load a static test pattern so the renderer/scaling/effects can be verified
    /// without a live device.
    LoadTestPattern,
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
    // Separate window support
    OpenInSeparateWindow, // Open streaming in a separate window
    // Virtual PETSCII keyboard
    ToggleVirtualKeyboard,
    VkSend(u8),     // inject one PETSCII byte
    VkModifier(u8), // toggle SHIFT / C= / CTRL (see virtual_keyboard::MOD_*)
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
    /// Stable numeric id shared cross-thread via `scale_mode_shared` and used as
    /// the fragment shader's effect selector.
    pub fn to_u8(self) -> u8 {
        match self {
            ScaleMode::PixelPerfect => 0,
            ScaleMode::Scale2x => 1,
            ScaleMode::Scanlines => 2,
            ScaleMode::CRT => 3,
            ScaleMode::Glow => 4,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => ScaleMode::PixelPerfect,
            1 => ScaleMode::Scale2x,
            2 => ScaleMode::Scanlines,
            3 => ScaleMode::CRT,
            4 => ScaleMode::Glow,
            _ => ScaleMode::PixelPerfect,
        }
    }
}

/// A static `VIC_WIDTH`×`VIC_HEIGHT` RGBA test pattern: 16 vertical C64 color
/// bars over the top, a horizontal luminance ramp below, and a 1px white grid so
/// integer scaling and the effects can be eyeballed without a live device.
fn test_pattern_rgba() -> Vec<u8> {
    let (w, h) = (VIC_WIDTH as usize, VIC_HEIGHT as usize);
    let mut buf = vec![0u8; w * h * 4];
    let bar_w = w / 16;
    let split = h / 2;
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = if y < split {
                let c = C64_PALETTE[(x / bar_w).min(15)];
                (c[0], c[1], c[2])
            } else {
                let v = (x * 255 / (w - 1)) as u8;
                (v, v, v)
            };
            // 1px grid every 16 source pixels.
            let grid = x % 16 == 0 || y % 16 == 0;
            let idx = (y * w + x) * 4;
            if grid {
                buf[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else {
                buf[idx..idx + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    buf
}

/// Apply a scale mode's pixel effect to a native frame, returning the RGBA bytes
/// and their dimensions. PixelPerfect is a passthrough (the GPU nearest-scales the
/// native frame); the effect modes run their CPU kernel to bake a 2× buffer. Used
/// by the compatibility render path and by screenshots — never per rendered frame
/// on the shader path.
fn render_effect(native: &[u8], mode: ScaleMode) -> (Vec<u8>, u32, u32) {
    use crate::video_scaling::{apply_crt_effect, apply_scanlines, scale2x};
    match mode {
        // Glow is a GPU-shader-only effect; the compatibility path shows plain.
        ScaleMode::PixelPerfect | ScaleMode::Glow => (native.to_vec(), VIC_WIDTH, VIC_HEIGHT),
        ScaleMode::Scale2x => (
            scale2x(native, VIC_WIDTH, VIC_HEIGHT),
            VIC_WIDTH * 2,
            VIC_HEIGHT * 2,
        ),
        ScaleMode::Scanlines => (
            apply_scanlines(native, VIC_WIDTH, VIC_HEIGHT),
            VIC_WIDTH * 2,
            VIC_HEIGHT * 2,
        ),
        ScaleMode::CRT => (
            apply_crt_effect(native, VIC_WIDTH, VIC_HEIGHT),
            VIC_WIDTH * 2,
            VIC_HEIGHT * 2,
        ),
    }
}

/// Build an image `Handle` for the compatibility (non-shader) render path. Only
/// called once per changed frame, never in `view()`.
fn build_compat_handle(native: &[u8], mode: ScaleMode) -> iced::widget::image::Handle {
    let (data, w, h) = render_effect(native, mode);
    iced::widget::image::Handle::from_rgba(w, h, data)
}

/// Pixel-perfect integer-fit display of `handle` (compatibility path): pick the
/// largest whole multiple of the handle's native size that fits the available
/// space, render at exactly that size with nearest sampling, centered with a
/// letterbox border. `native` is the handle's own pixel size.
fn vic_image_responsive<'a>(
    handle: iced::widget::image::Handle,
    native: (u32, u32),
) -> Element<'a, StreamingMessage> {
    let (nw, nh) = (native.0 as f32, native.1 as f32);
    responsive(move |size| {
        let scale = (size.width / nw)
            .floor()
            .min((size.height / nh).floor())
            .max(1.0);
        container(
            iced_image(handle.clone())
                .width(nw * scale)
                .height(nh * scale)
                .filter_method(FilterMethod::Nearest),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    })
    .into()
}

/// "Waiting for frames" placeholder shared by the video views.
fn waiting_placeholder<'a>(size: u32) -> Element<'a, StreamingMessage> {
    text("Waiting for frames...")
        .size(size as f32)
        .color(iced::Color::WHITE)
        .into()
}

/// Inject one PETSCII byte into the device's KERNAL keyboard buffer: clear
/// LSTX/NDX ($C5/$C6), write the byte to the buffer head ($0277), set NDX=1 so
/// the KERNAL processes it. Only needs a live connection (no streaming required).
/// Shared by the physical-key capture and the virtual keyboard.
fn send_petscii(
    connection: Option<Arc<Mutex<dyn RemoteDevice>>>,
    code: u8,
) -> Task<StreamingMessage> {
    let Some(conn) = connection else {
        return Task::none();
    };
    Task::perform(
        async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                tokio::task::spawn_blocking(move || {
                    let c = conn.lock().unwrap();
                    c.write_mem(0x00C5, &[0, 0])
                        .map_err(|e| format!("Clear failed: {}", e))?;
                    c.write_mem(0x0277, &[code])
                        .map_err(|e| format!("Buffer write failed: {}", e))?;
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
    )
}

/// One icon button for the video overlay bar. `msg = None` renders it disabled.
/// `active` highlights the button (e.g. audio/keyboard enabled).
fn overlay_button(
    glyph: &'static str,
    msg: Option<StreamingMessage>,
    active: bool,
    tip: &'static str,
    fs: &crate::styles::FontSizes,
) -> Element<'static, StreamingMessage> {
    let mut btn = button(text(glyph).size(fs.normal).color(iced::Color::WHITE))
        .padding([4, 8])
        .style(if active {
            button::primary
        } else {
            button::text
        });
    if let Some(m) = msg {
        btn = btn.on_press(m);
    }
    tooltip(btn, text(tip).size(fs.small), tooltip::Position::Top)
        .style(container::bordered_box)
        .into()
}

/// A uniform-width Video-Scale mode button (fills its cell so the buttons align
/// in a tidy grid), highlighted when it's the active mode.
fn mode_button(
    selected: ScaleMode,
    mode: ScaleMode,
    label: &'static str,
    tip: &'static str,
    fs: &crate::styles::FontSizes,
) -> Element<'static, StreamingMessage> {
    tooltip(
        button(text(label).size(fs.tiny))
            .on_press(StreamingMessage::ScaleModeChanged(mode))
            .padding([4, 6])
            .width(Length::Fill)
            .style(if selected == mode {
                button::primary
            } else {
                button::secondary
            }),
        text(tip).size(fs.small),
        tooltip::Position::Bottom,
    )
    .style(container::bordered_box)
    .into()
}

/// Native dimensions of the handle `build_compat_handle` produces for `mode`.
fn compat_handle_dimensions(mode: ScaleMode) -> (u32, u32) {
    match mode {
        ScaleMode::PixelPerfect | ScaleMode::Glow => (VIC_WIDTH, VIC_HEIGHT),
        _ => (VIC_WIDTH * 2, VIC_HEIGHT * 2),
    }
}

/// One assembled VIC frame at native resolution (always `VIC_WIDTH`×`VIC_HEIGHT`).
///
/// Scaling and pixel effects are applied on the GPU at render time (shader path)
/// or lazily at display/screenshot time (compatibility path) — never per-frame on
/// the stream thread. `version` increases on every stored frame so the renderer
/// can skip redundant work / GPU uploads when the frame hasn't changed. `data` is
/// `Arc`-wrapped so both the display and the shader program share it without
/// copying the ~417 KB buffer.
#[derive(Clone)]
pub struct NativeFrame {
    pub data: Arc<Vec<u8>>,
    pub version: u64,
}

pub struct VideoStreaming {
    pub is_streaming: bool,
    pub frame_buffer: Arc<Mutex<Option<NativeFrame>>>, // Native RGBA, produced by the stream thread
    pub current_handle: Option<iced::widget::image::Handle>, // Compatibility-path image handle
    /// Latest native frame + its version, sampled from `frame_buffer` in
    /// `FrameUpdate`. The shader program reads these directly.
    pub current_frame: Option<Arc<Vec<u8>>>,
    pub current_version: u64,
    /// When true, render via the wgpu shader widget; when false, use the
    /// compatibility responsive-image path (works on the tiny-skia fallback).
    pub use_gpu_shader: bool,
    pub current_dimensions: (u32, u32), // Dimensions of current handle
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
    // Virtual on-screen PETSCII keyboard
    show_virtual_keyboard: bool,
    vk_shift: bool,
    vk_comm: bool,
    vk_ctrl: bool,
    vk_glyphs: Vec<iced::widget::image::Handle>, // char-ROM glyphs, built on first show
    last_key_time: Option<std::time::Instant>,   // Rate limiting for keyboard
    // Ultimate64 host for stream control (binary protocol on port 64)
    pub ultimate_host: Option<String>,
    // API password for REST API fallback
    pub api_password: Option<String>,
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
            current_frame: None,
            current_version: 0,
            // GPU shader path by default; the Compatibility toggle switches to the
            // image path for machines on iced's tiny-skia fallback. Overridden
            // from persisted settings at startup.
            use_gpu_shader: true,
            current_dimensions: (VIC_WIDTH, VIC_HEIGHT),
            stop_signal: Arc::new(AtomicBool::new(false)),
            stream_handle: None,
            audio_stream_handle: None,
            audio_network_handle: None,
            command_input: String::new(),
            command_history: Vec::new(),
            stream_mode: StreamMode::Unicast,
            scale_mode: ScaleMode::PixelPerfect,
            listen_port: "11000".to_string(),
            packets_received: Arc::new(Mutex::new(0)),
            audio_packets_received: Arc::new(Mutex::new(0)),
            audio_enabled: true,
            audio_buffer: None,
            is_fullscreen: false,
            last_click_time: None,
            keyboard_enabled: false,
            show_virtual_keyboard: false,
            vk_shift: false,
            vk_comm: false,
            vk_ctrl: false,
            vk_glyphs: Vec::new(),
            last_key_time: None,
            ultimate_host: None,
            api_password: None,
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

    /// Preload the static test pattern (QA hook — lets the video path render
    /// without a device, e.g. when `U64_AUTO_TEST_PATTERN` is set at launch).
    pub fn preload_test_pattern(&mut self) {
        let pattern = Arc::new(test_pattern_rgba());
        self.current_version = self.current_version.wrapping_add(1);
        self.current_frame = Some(pattern);
        if !self.use_gpu_shader {
            if let Some(frame) = &self.current_frame {
                self.current_handle = Some(build_compat_handle(frame, self.scale_mode));
                self.current_dimensions = compat_handle_dimensions(self.scale_mode);
            }
        }
    }
    pub fn update_impl(
        &mut self,
        message: StreamingMessage,
        connection: Option<Arc<Mutex<dyn RemoteDevice>>>,
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
                // Sample the latest native frame once per tick (not per view()).
                // Skip everything if the frame hasn't changed since last tick.
                let latest = self.frame_buffer.lock().ok().and_then(|fb| fb.clone());
                if let Some(frame) = latest {
                    if frame.version != self.current_version {
                        self.current_version = frame.version;
                        self.current_frame = Some(frame.data.clone()); // Arc clone (cheap)
                                                                       // The compatibility (image-widget) path needs a Handle;
                                                                       // the shader path reads `current_frame` directly. Effects
                                                                       // in compatibility mode are applied here, off the hot path.
                        if !self.use_gpu_shader {
                            self.current_handle =
                                Some(build_compat_handle(&frame.data, self.scale_mode));
                            self.current_dimensions = compat_handle_dimensions(self.scale_mode);
                        }
                    }
                }
                Task::none()
            }
            StreamingMessage::TakeScreenshot => {
                if self.is_streaming {
                    // Fast path: grab the latest native frame from the stream buffer
                    // and bake the current effect into it once, at capture time.
                    let frame_data = if let Ok(fb_guard) = self.frame_buffer.lock() {
                        fb_guard.clone()
                    } else {
                        None
                    };

                    if let Some(frame) = frame_data {
                        let (data, w, h) = render_effect(&frame.data, self.scale_mode);
                        Task::perform(
                            save_screenshot_to_pictures(data, w, h),
                            StreamingMessage::ScreenshotComplete,
                        )
                    } else {
                        Task::perform(
                            async { Err("No frame available".to_string()) },
                            StreamingMessage::ScreenshotComplete,
                        )
                    }
                } else {
                    // Slow path: capture via REST API without starting streaming.
                    // Requires a connected host.
                    if let Some(host) = self.ultimate_host.clone() {
                        let password = self.api_password.clone();
                        Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    crate::screenshot_api::capture_screenshot_via_api(
                                        &host, password,
                                    )
                                })
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()))
                            },
                            StreamingMessage::ScreenshotComplete,
                        )
                    } else {
                        Task::perform(
                            async { Err("Not connected to Ultimate64".to_string()) },
                            StreamingMessage::ScreenshotComplete,
                        )
                    }
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
                // Rebuild the compatibility handle immediately so a mode switch is
                // visible without waiting for the next frame (shader path reads the
                // mode live in view()).
                if !self.use_gpu_shader {
                    if let Some(frame) = &self.current_frame {
                        self.current_handle = Some(build_compat_handle(frame, mode));
                        self.current_dimensions = compat_handle_dimensions(mode);
                    }
                }
                Task::none()
            }
            StreamingMessage::GpuShaderToggled(on) => {
                self.use_gpu_shader = on;
                // Switching to the compatibility path needs a handle built from the
                // current frame right away.
                if !on {
                    if let Some(frame) = &self.current_frame {
                        self.current_handle = Some(build_compat_handle(frame, self.scale_mode));
                        self.current_dimensions = compat_handle_dimensions(self.scale_mode);
                    }
                }
                Task::none()
            }
            StreamingMessage::LoadTestPattern => {
                let pattern = Arc::new(test_pattern_rgba());
                self.current_version = self.current_version.wrapping_add(1);
                self.current_frame = Some(pattern);
                if !self.use_gpu_shader {
                    if let Some(frame) = &self.current_frame {
                        self.current_handle = Some(build_compat_handle(frame, self.scale_mode));
                        self.current_dimensions = compat_handle_dimensions(self.scale_mode);
                    }
                }
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
                    return send_petscii(connection, code);
                }
                log::trace!("KEYBOARD: Key {:?} not mapped", key);
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

            StreamingMessage::OpenInSeparateWindow => {
                // Handled by main.rs which manages window creation
                Task::none()
            }

            StreamingMessage::ToggleVirtualKeyboard => {
                self.show_virtual_keyboard = !self.show_virtual_keyboard;
                // Build the glyph atlas once, on first show.
                if self.show_virtual_keyboard && self.vk_glyphs.is_empty() {
                    self.vk_glyphs = crate::virtual_keyboard::build_glyphs();
                }
                Task::none()
            }
            StreamingMessage::VkModifier(id) => {
                match id {
                    crate::virtual_keyboard::MOD_SHIFT => self.vk_shift = !self.vk_shift,
                    crate::virtual_keyboard::MOD_COMMODORE => self.vk_comm = !self.vk_comm,
                    _ => self.vk_ctrl = !self.vk_ctrl,
                }
                Task::none()
            }
            StreamingMessage::VkSend(code) => {
                // One-shot modifiers: applied to this key, then released — much
                // less confusing than a latched SHIFT that turns everything into
                // graphics.
                self.vk_shift = false;
                self.vk_comm = false;
                self.vk_ctrl = false;
                send_petscii(connection, code)
            }
        }
    }

    /// The live video display element — GPU shader path or compatibility image
    /// path — wrapped in a `mouse_area` for click-to-toggle-fullscreen. Shared by
    /// all three views. Shows a "waiting" placeholder until the first frame lands.
    fn video_element(&self, placeholder_size: u32) -> Element<'_, StreamingMessage> {
        let inner: Element<'_, StreamingMessage> = if self.use_gpu_shader {
            match &self.current_frame {
                Some(frame) => crate::vic_shader::vic_video(
                    frame.clone(),
                    self.current_version,
                    self.scale_mode,
                ),
                None => waiting_placeholder(placeholder_size),
            }
        } else {
            match &self.current_handle {
                // PixelPerfect gets crisp integer-fit; effect modes keep the
                // fill/contain path (their output is already a baked 2× buffer).
                Some(handle) if self.scale_mode == ScaleMode::PixelPerfect => {
                    vic_image_responsive(handle.clone(), self.current_dimensions)
                }
                Some(handle) => iced_image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(iced::ContentFit::Contain)
                    .filter_method(FilterMethod::Nearest)
                    .into(),
                None => waiting_placeholder(placeholder_size),
            }
        };
        mouse_area(inner)
            .on_press(StreamingMessage::VideoClicked)
            .into()
    }

    /// Media-player style control bar overlaid on the bottom of the video:
    /// play/stop, screenshot, fullscreen, pop-out, audio, keyboard.
    fn video_overlay_bar(&self, fs: &crate::styles::FontSizes) -> Element<'_, StreamingMessage> {
        let live_stop = if self.is_streaming {
            overlay_button(
                "■",
                Some(StreamingMessage::StopStream),
                false,
                "Stop stream",
                fs,
            )
        } else {
            overlay_button(
                "▶",
                Some(StreamingMessage::StartStream),
                false,
                "Start stream",
                fs,
            )
        };
        let shot = overlay_button(
            "📸",
            self.is_streaming
                .then_some(StreamingMessage::TakeScreenshot),
            false,
            if self.is_streaming {
                "Capture frame to Pictures"
            } else {
                "Start streaming to capture"
            },
            fs,
        );
        let full = overlay_button(
            "⛶",
            Some(StreamingMessage::ToggleFullscreen),
            self.is_fullscreen,
            "Fullscreen",
            fs,
        );
        let popout = overlay_button(
            "⧉",
            Some(StreamingMessage::OpenInSeparateWindow),
            false,
            "Open in a separate window",
            fs,
        );
        let audio = overlay_button(
            "🔊",
            Some(StreamingMessage::AudioToggled(!self.audio_enabled)),
            self.audio_enabled,
            "Toggle audio",
            fs,
        );
        let keyboard = overlay_button(
            "⌨",
            self.is_streaming
                .then_some(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled)),
            self.keyboard_enabled,
            "Capture the PC keyboard (type into the C64 while streaming)",
            fs,
        );
        let vkbd = overlay_button(
            "🎹",
            Some(StreamingMessage::ToggleVirtualKeyboard),
            self.show_virtual_keyboard,
            "Show/hide the on-screen PETSCII keyboard",
            fs,
        );

        let bar = row![
            live_stop,
            shot,
            full,
            popout,
            Space::new().width(Length::Fill),
            vkbd,
            audio,
            keyboard,
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        container(bar)
            .width(Length::Fill)
            .padding([6, 10])
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba(
                    0.0, 0.0, 0.0, 0.55,
                ))),
                border: iced::Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    }

    /// Fullscreen view - video fills the entire available space with black letterboxing
    pub fn view_fullscreen(&self, font_size: u32) -> Element<'_, StreamingMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let video_content: Element<'_, StreamingMessage> =
            if self.is_streaming || self.current_frame.is_some() {
                self.video_element(fs.header + 2)
            } else {
                text("Stream not active - press ESC to exit")
                    .size(fs.header + 2)
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
            .size(fs.normal),
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
                button(text("Exit Fullscreen (ESC or double-click)").size(fs.normal))
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

    /// Separate window view - similar to fullscreen but for dedicated streaming window
    pub fn view_separate_window(&self, font_size: u32) -> Element<'_, StreamingMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let video_content: Element<'_, StreamingMessage> =
            if self.is_streaming || self.current_frame.is_some() {
                self.video_element(fs.header + 2)
            } else {
                text("Stream not active")
                    .size(fs.header + 2)
                    .color(iced::Color::WHITE)
                    .into()
            };

        // Controls at the top with keyboard toggle
        let keyboard_btn = button(
            text(if self.keyboard_enabled {
                "⌨ Enabled"
            } else {
                "⌨ Disabled"
            })
            .size(fs.normal),
        )
        .on_press(StreamingMessage::ToggleKeyboard(!self.keyboard_enabled))
        .padding([6, 12])
        .style(if self.keyboard_enabled {
            button::primary
        } else {
            button::secondary
        });

        let fullscreen_btn = button(text("Fullscreen").size(fs.normal))
            .on_press(StreamingMessage::ToggleFullscreen)
            .padding([6, 12]);

        let controls = container(row![keyboard_btn, fullscreen_btn,].spacing(10))
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding(10);

        // Black background container with centered video
        container(column![
            controls,
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

    pub fn view(&self, font_size: u32) -> Element<'_, StreamingMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        // === LEFT SIDE: Video display (fluid scaling) ===
        // Base video content (the picture, or the inactive message).
        let video_base: Element<'_, StreamingMessage> = if self.is_streaming
            || self.current_frame.is_some()
        {
            container(self.video_element(fs.large))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        } else {
            let status_info = match self.stream_mode {
                StreamMode::Unicast => {
                    "Unicast: the device streams here once you press ▶".to_string()
                }
                StreamMode::Multicast => "Multicast 239.0.1.64 (requires wired LAN)".to_string(),
            };
            container(
                column![
                    text("VIDEO STREAM INACTIVE").size(fs.large + 2),
                    Space::new().height(10),
                    text(status_info).size(fs.small),
                    Space::new().height(5),
                    text("Press ▶ below to begin streaming").size(fs.small),
                ]
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        };

        // Bottom overlay stack on the video: the on-screen keyboard (if shown)
        // sits just above the media-player control bar, both floating on the video.
        let mut overlay_col = column![].spacing(6).align_x(iced::Alignment::Center);
        if self.show_virtual_keyboard {
            overlay_col = overlay_col.push(crate::virtual_keyboard::view(
                &self.vk_glyphs,
                self.vk_shift,
                self.vk_comm,
                self.vk_ctrl,
                &fs,
            ));
        }
        overlay_col = overlay_col.push(self.video_overlay_bar(&fs));

        let video_display: Element<'_, StreamingMessage> = stack![
            video_base,
            container(overlay_col)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Bottom)
                .padding(8),
        ]
        .into();

        // === RIGHT SIDE: Controls panel ===
        let dim = iced::Color::from_rgb(0.55, 0.57, 0.62);

        let source_button = |label: &'static str, mode: StreamMode| {
            button(text(label).size(fs.small))
                .on_press(StreamingMessage::StreamModeChanged(mode))
                .padding([4, 8])
                .width(Length::Fill)
                .style(if self.stream_mode == mode {
                    button::primary
                } else {
                    button::secondary
                })
        };

        // SOURCE — stream mode + port
        let mode_section = column![
            text("SOURCE").size(fs.small).color(dim),
            row![
                source_button("Unicast", StreamMode::Unicast),
                source_button("Multicast", StreamMode::Multicast),
            ]
            .spacing(4),
            row![
                text("Port").size(fs.small).color(dim),
                text_input("11000", &self.listen_port)
                    .on_input(StreamingMessage::PortChanged)
                    .width(Length::Fill)
                    .size(fs.small),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(6);

        // Scale mode selection — uniform-width buttons in a tidy 2-2-1 grid.
        let sm = self.scale_mode;
        let scale_section = column![
            text("DISPLAY").size(fs.small).color(dim),
            row![
                mode_button(
                    sm,
                    ScaleMode::PixelPerfect,
                    "Pixel Perfect",
                    "Sharp integer scaling (largest whole multiple that fits)",
                    &fs,
                ),
                mode_button(
                    sm,
                    ScaleMode::Scale2x,
                    "Smooth",
                    "Scale2x edge smoothing",
                    &fs
                ),
            ]
            .spacing(4),
            row![
                mode_button(
                    sm,
                    ScaleMode::Scanlines,
                    "Scanlines",
                    "Soft CRT scanlines",
                    &fs,
                ),
                mode_button(
                    sm,
                    ScaleMode::CRT,
                    "CRT",
                    "CRT: curvature + shadow mask + scanlines",
                    &fs,
                ),
            ]
            .spacing(4),
            mode_button(
                sm,
                ScaleMode::Glow,
                "Glow",
                "Phosphor bloom on a curved CRT (GPU renderer only)",
                &fs,
            ),
            tooltip(
                row![
                    checkbox(self.use_gpu_shader)
                        .on_toggle(StreamingMessage::GpuShaderToggled)
                        .size(fs.small as f32),
                    text("GPU renderer").size(fs.small),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
                text(
                    "GPU shader (crisp, fast). Uncheck for the compatibility renderer \
                     if video doesn't appear (no-GPU / tiny-skia fallback)."
                )
                .size(fs.small),
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Test Pattern").size(fs.tiny))
                    .on_press(StreamingMessage::LoadTestPattern)
                    .padding([4, 6])
                    .width(Length::Fill)
                    .style(button::secondary),
                text("Show a static test pattern to verify the renderer without a device")
                    .size(fs.small),
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(6);

        // Right panel — SOURCE + DISPLAY (the console now lives in the bottom bar).
        let right_panel = container(
            column![mode_section, rule::horizontal(1), scale_section,]
                .spacing(12)
                .padding(10)
                .width(Length::Fixed(210.0)),
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

        // On-screen PETSCII keyboard (toggled from the overlay 🎹 button).
        // Command console, moved to a full-width bar at the bottom.
        let console_bar = row![
            text("C64>").size(fs.small).color(dim),
            text_input("Enter BASIC command…", &self.command_input)
                .on_input(StreamingMessage::CommandInputChanged)
                .on_submit(StreamingMessage::SendCommand)
                .width(Length::Fill)
                .size(fs.small),
            button(text("Send").size(fs.small))
                .on_press(StreamingMessage::SendCommand)
                .padding([4, 14]),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);

        column![
            text("VIC VIDEO STREAM").size(fs.header + 2),
            rule::horizontal(1),
            main_content,
            console_bar,
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    pub fn subscription(&self) -> Subscription<StreamingMessage> {
        let mut subscriptions = Vec::new();

        // Sample the latest frame on a fixed timer that ticks a bit faster than
        // the source (~50/60 fps) so no frame is skipped. A timer actively forces
        // a redraw each tick; window::frames() only reports redraws iced already
        // decided to do, which under-drove the display rate. FrameUpdate is a cheap
        // no-op when the frame version is unchanged.
        if self.is_streaming {
            subscriptions.push(
                iced::time::every(Duration::from_millis(12)).map(|_| StreamingMessage::FrameUpdate),
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
                    let multicast_addr = MULTICAST_VIDEO;
                    let interface = MULTICAST_IFACE;
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

        // Enlarge the kernel receive buffer so bursts of a frame's packets aren't
        // dropped (each VIC frame is many UDP packets at ~50 fps). The OS may clamp
        // the request; that's fine — we still get more than the small default.
        {
            let sock_ref = socket2::SockRef::from(&socket);
            let want = 4 * 1024 * 1024; // 4 MB
            match sock_ref.set_recv_buffer_size(want) {
                Ok(()) => log::info!(
                    "UDP recv buffer: requested {} B, got {:?} B",
                    want,
                    sock_ref.recv_buffer_size()
                ),
                Err(e) => log::warn!("Could not enlarge UDP recv buffer: {}", e),
            }
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
            let mut frame_version: u64 = 0;

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

                        // Frame complete — publish the native frame. Scaling and
                        // effects happen at render time (GPU shader) or lazily in
                        // the compatibility path, never here on the stream thread.
                        if is_frame_end {
                            frame_version = frame_version.wrapping_add(1);
                            // One copy out of the reused assembly buffer, wrapped in
                            // an Arc the display and shader share without recopying.
                            let snapshot = Arc::new(rgba_frame.clone());
                            if let Ok(mut fb) = frame_buffer.lock() {
                                *fb = Some(NativeFrame {
                                    data: snapshot,
                                    version: frame_version,
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
                let multicast_addr = MULTICAST_VIDEO;
                let interface = MULTICAST_IFACE;
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
                        let multicast_addr = MULTICAST_AUDIO;
                        let interface = MULTICAST_IFACE;
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
                let multicast_addr = MULTICAST_AUDIO;
                let interface = MULTICAST_IFACE;
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
                let multicast_addr = MULTICAST_VIDEO;
                let interface = MULTICAST_IFACE;
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

impl crate::tab::TabController for VideoStreaming {
    type Message = StreamingMessage;
    fn update(
        &mut self,
        message: StreamingMessage,
        ctx: crate::tab::TabContext,
    ) -> iced::Task<StreamingMessage> {
        self.update_impl(message, ctx.connection)
    }
}
