use iced::{
    widget::{button, checkbox, column, container, row, scrollable, text, text_input, Column, Space, 
             image as iced_image},
    Command, Element, Length, Subscription,
};
use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

// Video frame dimensions
pub const VIC_WIDTH: u32 = 384;
pub const VIC_HEIGHT: u32 = 272;
const FRAME_SIZE: usize = (VIC_WIDTH * VIC_HEIGHT) as usize; // 104448 bytes

// Audio constants
const AUDIO_PORT_OFFSET: u16 = 1;  // Audio port = video port + 1
const AUDIO_SAMPLE_RATE: u32 = 48000;
const AUDIO_CHANNELS: u16 = 2;
const AUDIO_SAMPLES_PER_PACKET: usize = 192 * 4;  // 768 samples (384 stereo pairs)
const AUDIO_HEADER_SIZE: usize = 2;  // Just sequence number
const AUDIO_BUFFER_SIZE: usize = AUDIO_SAMPLE_RATE as usize;  // ~1 second buffer

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
    StreamModeChanged(StreamMode),
    PortChanged(String),
    AudioToggled(bool),
}

pub struct VideoStreaming {
    pub is_streaming: bool,
    pub frame_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub image_buffer: Arc<Mutex<Option<Vec<u8>>>>,
    pub stop_signal: Arc<AtomicBool>,
    stream_handle: Option<thread::JoinHandle<()>>,
    audio_stream_handle: Option<thread::JoinHandle<()>>,
    pub command_input: String,
    pub command_history: Vec<String>,
    pub stream_mode: StreamMode,
    pub listen_port: String,
    pub status_message: Option<String>,
    pub packets_received: Arc<Mutex<u64>>,
    pub audio_packets_received: Arc<Mutex<u64>>,
    pub audio_enabled: bool,
    audio_producer: Option<Arc<Mutex<VecDeque<i16>>>>,
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
            command_input: String::new(),
            command_history: Vec::new(),
            stream_mode: StreamMode::Unicast,
            listen_port: "11000".to_string(),
            status_message: None,
            packets_received: Arc::new(Mutex::new(0)),
            audio_packets_received: Arc::new(Mutex::new(0)),
            audio_enabled: true,
            audio_producer: None,
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
                let port = self.listen_port.parse().unwrap_or(11000);
                let mode = self.stream_mode;
                Command::perform(
                    take_screenshot_async(port, mode),
                    StreamingMessage::ScreenshotComplete
                )
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
                if !self.command_input.is_empty() {
                    self.command_history.push(self.command_input.clone());
                    self.command_input.clear();
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
        }
    }

    pub fn view(&self) -> Element<'_, StreamingMessage> {
        // Mode selection
        let mode_row = row![
            text("Mode:").size(12),
            button(text("Unicast").size(11))
                .on_press(StreamingMessage::StreamModeChanged(StreamMode::Unicast))
                .padding([4, 8])
                .style(if self.stream_mode == StreamMode::Unicast {
                    iced::theme::Button::Primary
                } else {
                    iced::theme::Button::Secondary
                }),
            button(text("Multicast").size(11))
                .on_press(StreamingMessage::StreamModeChanged(StreamMode::Multicast))
                .padding([4, 8])
                .style(if self.stream_mode == StreamMode::Multicast {
                    iced::theme::Button::Primary
                } else {
                    iced::theme::Button::Secondary
                }),
            text("Port:").size(12),
            text_input("11000", &self.listen_port)
                .on_input(StreamingMessage::PortChanged)
                .width(Length::Fixed(60.0))
                .size(12),
        ]
        .spacing(8)
        .align_items(iced::Alignment::Center);

        let controls = row![
            if self.is_streaming {
                button(text("STOP"))
                    .on_press(StreamingMessage::StopStream)
                    .padding([8, 16])
            } else {
                button(text("START"))
                    .on_press(StreamingMessage::StartStream)
                    .padding([8, 16])
            },
            button(text("Screenshot"))
                .on_press(StreamingMessage::TakeScreenshot)
                .padding([8, 16]),
            checkbox("Audio", self.audio_enabled)
                .on_toggle(StreamingMessage::AudioToggled)
                .size(16)
                .text_size(12),
        ]
        .spacing(10)
        .align_items(iced::Alignment::Center);

        // Status info
        let video_packets = self.packets_received.lock().map(|p| *p).unwrap_or(0);
        let audio_packets = self.audio_packets_received.lock().map(|p| *p).unwrap_or(0);
        let status_info = if self.is_streaming {
            format!("Video: {} pkts | Audio: {} pkts | Mode: {}", video_packets, audio_packets, self.stream_mode)
        } else {
            match self.stream_mode {
                StreamMode::Unicast => format!(
                    "Unicast mode: Configure Ultimate64 to send to YOUR_MAC_IP:{}",
                    self.listen_port
                ),
                StreamMode::Multicast => "Multicast mode: 239.0.1.64 (requires wired LAN)".to_string(),
            }
        };

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
                    
                    container(
                        column![
                            iced_image(handle)
                                .width(Length::Fixed((VIC_WIDTH * 2) as f32))
                                .height(Length::Fixed((VIC_HEIGHT * 2) as f32))
                                .content_fit(iced::ContentFit::Fill),
                            text(format!("384x272 | Video: {} | Audio: {}", video_packets, audio_packets)).size(10),
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
                                    text(format!("Video: {} | Audio: {}", video_packets, audio_packets)).size(12),
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
                                    text(format!("Video: {} | Audio: {}", video_packets, audio_packets)).size(12),
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
            .padding(40)
            .into()
        };

        // Command prompt section
        let command_history_items: Vec<Element<'_, StreamingMessage>> = self.command_history
            .iter()
            .rev()
            .take(10)
            .map(|cmd| text(format!("> {}", cmd)).size(11).into())
            .collect();

        let command_section = column![
            text("COMMAND PROMPT").size(14),
            row![
                text("C64> ").size(14),
                text_input("Enter BASIC command...", &self.command_input)
                    .on_input(StreamingMessage::CommandInputChanged)
                    .on_submit(StreamingMessage::SendCommand)
                    .width(Length::Fill),
                button(text("Send").size(12))
                    .on_press(StreamingMessage::SendCommand)
                    .padding([4, 12]),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center),
            // Command history
            scrollable(
                Column::with_children(command_history_items)
                    .spacing(2),
            )
            .height(Length::Fixed(100.0)),
        ]
        .spacing(10)
        .padding(10);

        column![
            text("VIC VIDEO STREAM").size(20),
            iced::widget::horizontal_rule(1),
            mode_row,
            Space::with_height(5),
            controls,
            video_display,
            iced::widget::horizontal_rule(1),
            command_section,
        ]
        .spacing(10)
        .into()
    }

    pub fn subscription(&self) -> Subscription<StreamingMessage> {
        if self.is_streaming {
            iced::time::every(Duration::from_millis(40))
                .map(|_| StreamingMessage::FrameUpdate)
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
                StreamMode::Unicast => {
                    // Bind to all interfaces on the specified port
                    match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                        Ok(s) => {
                            log::info!("Unicast socket bound to 0.0.0.0:{}", port);
                            s
                        }
                        Err(e) => {
                            log::error!("Failed to bind unicast socket: {}", e);
                            return;
                        }
                    }
                }
                StreamMode::Multicast => {
                    // Create multicast socket
                    match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                        Ok(s) => {
                            // Join multicast group
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
                    }
                }
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
            // Low nibble = LEFT pixel (x*2), High nibble = RIGHT pixel (x*2+1)
            let mut color_lut: Vec<[u8; 8]> = Vec::with_capacity(256);
            for i in 0..256 {
                let hi = (i >> 4) & 0x0F;  // High nibble = RIGHT pixel
                let lo = i & 0x0F;          // Low nibble = LEFT pixel
                let c_hi = &C64_PALETTE[hi];
                let c_lo = &C64_PALETTE[lo];
                color_lut.push([
                    c_lo[0], c_lo[1], c_lo[2], 255,  // LEFT pixel (low nibble) - first in memory
                    c_hi[0], c_hi[1], c_hi[2], 255,  // RIGHT pixel (high nibble) - second in memory
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
                        let pixels_in_line = u16::from_le_bytes([recv_buf[6], recv_buf[7]]) as usize;
                        let lines_in_packet = recv_buf[8] as usize;
                        
                        // Log first packet info
                        if first_packet {
                            first_packet = false;
                            log::info!("First packet: pixels_in_line={}, lines_in_packet={}, payload_size={}", 
                                pixels_in_line, lines_in_packet, size - HEADER_SIZE);
                        }
                        
                        let line_num = (line_raw & 0x7FFF) as usize;  // Strip MSB (sync flag)
                        let is_frame_end = (line_raw & 0x8000) != 0;
                        
                        let payload = &recv_buf[HEADER_SIZE..size];
                        let bytes_per_line = pixels_in_line / 2;  // 2 pixels per byte = 192 bytes/line
                        
                        // Process each line in the packet
                        for l in 0..lines_in_packet {
                            let y = line_num + l;
                            if y >= VIC_HEIGHT as usize {
                                continue;
                            }
                            
                            let payload_offset = l * bytes_per_line;
                            
                            // Write pixels to RGBA buffer using VIC_WIDTH stride
                            let row_offset = y * (VIC_WIDTH as usize) * 4;
                            
                            for x in 0..bytes_per_line {
                                if payload_offset + x >= payload.len() {
                                    break;
                                }
                                let packed_byte = payload[payload_offset + x] as usize;
                                let colors = &color_lut[packed_byte];
                                
                                // Each packed byte = 2 pixels
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
        
        // Create shared audio buffer
        let audio_buffer: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_BUFFER_SIZE)));
        self.audio_producer = Some(audio_buffer.clone());
        
        let consumer_buffer = audio_buffer.clone();
        let stop_signal = self.stop_signal.clone();
        let audio_packets_counter = self.audio_packets_received.clone();
        
        // Start audio output thread using cpal
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
            
            log::info!("Using audio device: {}", device.name().unwrap_or_default());
            
            let config = cpal::StreamConfig {
                channels: AUDIO_CHANNELS,
                sample_rate: cpal::SampleRate(AUDIO_SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Fixed(512),
            };
            
            let consumer = consumer_buffer;
            let stream = match device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    // Fill output buffer from shared buffer
                    if let Ok(mut buf) = consumer.lock() {
                        for sample in data.iter_mut() {
                            *sample = buf.pop_front().unwrap_or(0);
                        }
                    }
                },
                |err| {
                    log::error!("Audio stream error: {}", err);
                },
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Failed to build audio stream: {}", e);
                    return;
                }
            };
            
            if let Err(e) = stream.play() {
                log::error!("Failed to start audio playback: {}", e);
                return;
            }
            
            log::info!("Audio playback started");
            
            // Keep thread alive while streaming
            while !stop_signal.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
            
            log::info!("Audio playback thread stopped");
        });
        
        // Start audio network receiver thread
        let producer_buffer = audio_buffer;
        let stop_signal_net = self.stop_signal.clone();
        let audio_packets_counter_clone = audio_packets_counter;
        
        let network_handle = thread::spawn(move || {
            log::info!("Audio network thread started on port {}", port);
            
            let socket = match mode {
                StreamMode::Unicast => {
                    match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
                        Ok(s) => {
                            log::info!("Audio unicast socket bound to 0.0.0.0:{}", port);
                            s
                        }
                        Err(e) => {
                            log::error!("Failed to bind audio socket: {}", e);
                            return;
                        }
                    }
                }
                StreamMode::Multicast => {
                    match UdpSocket::bind(format!("0.0.0.0:{}", port)) {
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
                    }
                }
            };
            
            if let Err(e) = socket.set_nonblocking(true) {
                log::error!("Failed to set audio socket non-blocking: {}", e);
                return;
            }
            
            let mut recv_buf = [0u8; 2048];
            
            loop {
                if stop_signal_net.load(Ordering::Relaxed) {
                    break;
                }
                
                match socket.recv_from(&mut recv_buf) {
                    Ok((size, _addr)) => {
                        if size < AUDIO_HEADER_SIZE {
                            continue;
                        }
                        
                        // Count packets
                        if let Ok(mut p) = audio_packets_counter_clone.lock() {
                            *p += 1;
                        }
                        
                        // Skip 2-byte sequence header, rest is i16 samples
                        let audio_data = &recv_buf[AUDIO_HEADER_SIZE..size];
                        
                        // Convert bytes to i16 samples (little-endian) and push to buffer
                        if let Ok(mut buf) = producer_buffer.lock() {
                            for chunk in audio_data.chunks_exact(2) {
                                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                                // Keep buffer size limited
                                if buf.len() < AUDIO_BUFFER_SIZE {
                                    buf.push_back(sample);
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
        
        // We'll just track the audio playback handle (network thread will stop via stop_signal)
        self.audio_stream_handle = Some(audio_handle);
        let _ = network_handle; // Network thread detaches
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
        
        // Stop audio thread
        if let Some(handle) = self.audio_stream_handle.take() {
            let _ = handle.join();
        }
        
        // Clear audio producer
        self.audio_producer = None;

        self.is_streaming = false;
        if let Ok(mut frame) = self.frame_buffer.lock() {
            *frame = None;
        }
        if let Ok(mut img) = self.image_buffer.lock() {
            *img = None;
        }
    }
}

impl Drop for VideoStreaming {
    fn drop(&mut self) {
        if self.is_streaming {
            self.stop_stream();
        }
    }
}

// Decode VIC stream frame to RGBA (used for raw frame data, not packet-based data)
#[allow(dead_code)]
fn decode_vic_frame(raw_data: &[u8]) -> Option<Vec<u8>> {
    let expected_indexed = (VIC_WIDTH * VIC_HEIGHT) as usize; // 104448 bytes (1 byte/pixel)
    let expected_rgb = expected_indexed * 3; // 313344 bytes (3 bytes/pixel)
    let expected_rgba = expected_indexed * 4; // 417792 bytes (4 bytes/pixel)
    
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
        // Unknown format but has enough data - try indexed interpretation
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
        log::warn!("Unknown frame format: {} bytes (expected {} or {} or {})", 
            raw_data.len(), expected_indexed, expected_rgb, expected_rgba);
        None
    }
}

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
            StreamMode::Unicast => {
                UdpSocket::bind(format!("0.0.0.0:{}", port))
                    .map_err(|e| format!("Failed to bind socket: {}", e))?
            }
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
        
        socket.set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("Failed to set timeout: {}", e))?;
        
        let mut recv_buf = [0u8; 1024];
        let rgba_size = (VIC_WIDTH * VIC_HEIGHT * 4) as usize;
        let mut rgba_frame: Vec<u8> = vec![0u8; rgba_size];
        
        // Build color lookup table (low nibble = LEFT, high nibble = RIGHT)
        let mut color_lut: Vec<[u8; 8]> = Vec::with_capacity(256);
        for i in 0..256 {
            let hi = (i >> 4) & 0x0F;
            let lo = i & 0x0F;
            let c_hi = &C64_PALETTE[hi];
            let c_lo = &C64_PALETTE[lo];
            color_lut.push([
                c_lo[0], c_lo[1], c_lo[2], 255,  // LEFT pixel (low nibble)
                c_hi[0], c_hi[1], c_hi[2], 255,  // RIGHT pixel (high nibble)
            ]);
        }
        
        // Wait for a complete frame (until we see frame end flag)
        let start = std::time::Instant::now();
        let mut got_frame = false;
        
        while !got_frame && start.elapsed() < Duration::from_secs(5) {
            match socket.recv_from(&mut recv_buf) {
                Ok((size, _)) => {
                    if size < HEADER_SIZE {
                        continue;
                    }
                    
                    // Parse header
                    let line_raw = u16::from_le_bytes([recv_buf[4], recv_buf[5]]);
                    let pixels_in_line = u16::from_le_bytes([recv_buf[6], recv_buf[7]]) as usize;
                    let lines_in_packet = recv_buf[8] as usize;
                    
                    let line_num = (line_raw & 0x7FFF) as usize;
                    let is_frame_end = (line_raw & 0x8000) != 0;
                    
                    let payload = &recv_buf[HEADER_SIZE..size];
                    let half_pixels = pixels_in_line / 2;
                    
                    // Process lines
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