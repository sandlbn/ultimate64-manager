// Debug bus-trace stream capture.
//
// The Ultimate64 can emit a cycle-accurate 6510/VIC/1541 bus trace over UDP
// (`PUT /v1/streams/debug:start`, firmware ≥ 3.7). The datagram sample layout
// is generated FPGA-side and is not part of the public REST/socket docs, so we
// deliberately do NOT try to decode it into VCD here — inventing a layout would
// produce silently-wrong waveforms. Instead we capture the raw datagram payload
// stream to a file, which the documented external converter turns into a
// GtkWave `.vcd`.
//
// Mechanically this mirrors `streaming.rs`: bind the UDP socket first, send the
// start command over the existing stream-control layer, then read datagrams on
// a background thread into a shared buffer with a stop flag and counters. The UI
// polls the counters on a timer.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::net_utils::get_local_ip;
use crate::settings::StreamControlMethod;
use crate::stream_control::{send_stop_command, send_stream_command};

/// Binary stream command id for the debug stream (see stream_control.rs).
const STREAM_CMD_DEBUG: u8 = 0x22;
/// Default UDP port for the debug stream (video=11000, audio=11001, debug=11002).
pub const DEFAULT_DEBUG_PORT: u16 = 11002;

/// Raw debug-stream capture engine. One instance lives in the app; `start`
/// spins up a reader thread and `stop` tears it down.
pub struct DebugStreamCapture {
    pub active: bool,
    pub listen_port: u16,
    /// Device host (bare IP/hostname, no scheme) and API password for the
    /// port-64 / REST stream-control command.
    ultimate_host: Option<String>,
    api_password: Option<String>,
    stream_control_method: StreamControlMethod,

    stop_signal: Arc<AtomicBool>,
    capture: Arc<Mutex<Vec<u8>>>,
    packets: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl Default for DebugStreamCapture {
    fn default() -> Self {
        Self {
            active: false,
            listen_port: DEFAULT_DEBUG_PORT,
            ultimate_host: None,
            api_password: None,
            stream_control_method: StreamControlMethod::default(),
            stop_signal: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(Mutex::new(Vec::new())),
            packets: Arc::new(AtomicU64::new(0)),
            bytes: Arc::new(AtomicU64::new(0)),
            handle: None,
        }
    }
}

impl DebugStreamCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the device connection parameters used for the start/stop command.
    /// `host` may include a scheme — it is stripped to a bare host.
    pub fn set_host(
        &mut self,
        host: Option<String>,
        password: Option<String>,
        method: StreamControlMethod,
    ) {
        self.ultimate_host = host.map(|h| {
            h.trim_start_matches("http://")
                .trim_start_matches("https://")
                .to_string()
        });
        self.api_password = password;
        self.stream_control_method = method;
    }

    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Number of bytes currently held in the capture buffer.
    pub fn captured_len(&self) -> usize {
        self.capture.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Start capturing. Returns Err with a message if the socket can't bind or
    /// no device host is configured.
    pub fn start(&mut self) -> Result<(), String> {
        if self.active {
            return Ok(());
        }
        let Some(ultimate_ip) = self.ultimate_host.clone() else {
            return Err("Not connected".to_string());
        };
        let port = self.listen_port;

        // Bind the socket BEFORE sending the start command (matches streaming.rs).
        let socket = UdpSocket::bind(format!("0.0.0.0:{}", port))
            .map_err(|e| format!("Failed to bind UDP :{} — {}", port, e))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("Socket config failed: {}", e))?;

        // Fresh capture + counters.
        self.stop_signal.store(false, Ordering::Relaxed);
        self.packets.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
        if let Ok(mut c) = self.capture.lock() {
            c.clear();
        }

        // Send the debug-stream start command to the device.
        let my_ip = get_local_ip().ok_or("Could not detect local IP address")?;
        // Ensure any prior debug stream is stopped first.
        let _ = send_stop_command(
            &ultimate_ip,
            STREAM_CMD_DEBUG,
            self.api_password.as_deref(),
            self.stream_control_method,
        );
        send_stream_command(
            &ultimate_ip,
            &my_ip,
            port,
            STREAM_CMD_DEBUG,
            self.api_password.as_deref(),
            self.stream_control_method,
        )
        .map_err(|e| format!("Failed to start debug stream: {}", e))?;

        // Reader thread: append raw datagram payloads to the capture buffer.
        let stop_signal = self.stop_signal.clone();
        let capture = self.capture.clone();
        let packets = self.packets.clone();
        let bytes = self.bytes.clone();
        let handle = thread::spawn(move || {
            // Debug datagrams are ~1444 bytes; use a generous buffer.
            let mut recv_buf = [0u8; 2048];
            loop {
                if stop_signal.load(Ordering::Relaxed) {
                    break;
                }
                match socket.recv_from(&mut recv_buf) {
                    Ok((size, _addr)) => {
                        if size == 0 {
                            continue;
                        }
                        packets.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(size as u64, Ordering::Relaxed);
                        if let Ok(mut c) = capture.lock() {
                            // Cap the in-memory capture at 256 MiB as a safety
                            // valve so a forgotten capture can't exhaust RAM.
                            const CAP: usize = 256 * 1024 * 1024;
                            if c.len() + size <= CAP {
                                c.extend_from_slice(&recv_buf[..size]);
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(e) => {
                        log::error!("Debug stream recv error: {}", e);
                        break;
                    }
                }
            }
            log::info!("Debug stream reader thread stopped");
        });

        self.handle = Some(handle);
        self.active = true;
        log::info!("Debug stream capture started on :{}", port);
        Ok(())
    }

    /// Stop capturing and send the stop command to the device. The captured
    /// bytes remain available via [`take_capture`] until the next `start`.
    pub fn stop(&mut self) {
        if !self.active {
            return;
        }
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Some(ultimate_ip) = &self.ultimate_host {
            let _ = send_stop_command(
                ultimate_ip,
                STREAM_CMD_DEBUG,
                self.api_password.as_deref(),
                self.stream_control_method,
            );
        }
        self.active = false;
        log::info!(
            "Debug stream capture stopped ({} packets, {} bytes)",
            self.packets(),
            self.bytes()
        );
    }

    /// Snapshot the current capture buffer (does not clear it).
    pub fn snapshot(&self) -> Vec<u8> {
        self.capture.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

/// Save a raw debug-stream capture to a file the user picks.
pub async fn save_capture_async(data: Vec<u8>) -> Result<String, String> {
    if data.is_empty() {
        return Err("Nothing captured yet".to_string());
    }
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Debug capture", &["bin", "u64dbg"])
        .set_file_name("debug-capture.bin")
        .save_file()
        .await
        .ok_or("Save cancelled")?;
    let path = handle.path().to_path_buf();
    tokio::fs::write(&path, &data)
        .await
        .map_err(|e| format!("Write error: {}", e))?;
    Ok(path.to_string_lossy().to_string())
}
