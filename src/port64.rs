//! Raw TCP port-64 client for the Ultimate 64 / Ultimate II+.
//!
//! Mirrors every command defined in `socket_dma.cc`.  The server listens on
//! TCP port 64; each call opens a fresh connection, optionally authenticates,
//! sends the command, reads any reply, then closes.
//!
//! # Wire protocol
//! ```text
//! ┌──────────┬──────────┬────────────────────┐
//! │ cmd (2 B)│ len (2 B)│ payload (len bytes) │
//! └──────────┴──────────┴────────────────────┘
//! ```
//! Both `cmd` and `len` are little-endian `u16`.
//! All command words are in the `0xFF00–0xFFFF` range.
//!
//! # Authentication
//! If a password is configured on the device every session must start with
//! `AUTHENTICATE` before any other command, otherwise the server silently
//! drops the connection.  [`Port64Client`] handles this automatically.
//!
//! # Usage
//! ```rust,no_run
//! use port64::Port64Client;
//!
//! let client = Port64Client::new("192.168.1.64", Some("secret".into()));
//! client.dma_write(0x0400, b"HELLO WORLD").await?;
//! client.reset().await?;
//! ```

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ─────────────────────────────────────────────────────────────────
//  Protocol constants  (mirrors socket_dma.cc #defines)
// ─────────────────────────────────────────────────────────────────

/// TCP port the Ultimate DMA service listens on.
pub const PORT: u16 = 64;

/// Default per-operation timeout in seconds. Applies to connect and to reads of
/// small replies — *not* to bulk image transfers, which use
/// [`IMAGE_TIMEOUT_SECS`].
pub const TIMEOUT_SECS: u64 = 5;

/// Budget for pushing a whole disk/cartridge image over the socket.
///
/// Images run from 175 KB (d64) to ~800 KB (d81), far beyond what the 5 s
/// per-operation default is meant to cover; matches the 60 s already used for
/// the REST multipart upload and the FTP fetch of the same images.
pub const IMAGE_TIMEOUT_SECS: u64 = 60;

// All 25 command codes from socket_dma.cc
pub const CMD_DMA: u16 = 0xFF01; // Load PRG via DMA
pub const CMD_DMARUN: u16 = 0xFF02; // Load + run via DMA
pub const CMD_KEYB: u16 = 0xFF03; // Inject keyboard buffer
pub const CMD_RESET: u16 = 0xFF04; // Soft-reset C64
pub const CMD_WAIT: u16 = 0xFF05; // Delay (len = ms)
pub const CMD_DMAWRITE: u16 = 0xFF06; // Raw write to C64 address
pub const CMD_REUWRITE: u16 = 0xFF07; // Write to REU address space
pub const CMD_KERNALWRITE: u16 = 0xFF08; // Replace active Kernal ROM
pub const CMD_DMAJUMP: u16 = 0xFF09; // Load + jump to address
pub const CMD_MOUNT_IMG: u16 = 0xFF0A; // Mount disk image
pub const CMD_RUN_IMG: u16 = 0xFF0B; // Mount + run disk image
pub const CMD_POWEROFF: u16 = 0xFF0C; // Power off C64
pub const CMD_RUN_CRT: u16 = 0xFF0D; // Mount + run cartridge image
pub const CMD_IDENTIFY: u16 = 0xFF0E; // Query device identity string
pub const CMD_AUTHENTICATE: u16 = 0xFF1F; // Password authentication

// U64-only streaming commands
pub const CMD_VICSTREAM_ON: u16 = 0xFF20;
pub const CMD_AUDIOSTREAM_ON: u16 = 0xFF21;
pub const CMD_DEBUGSTREAM_ON: u16 = 0xFF22;
pub const CMD_VICSTREAM_OFF: u16 = 0xFF30;
pub const CMD_AUDIOSTREAM_OFF: u16 = 0xFF31;
pub const CMD_DEBUGSTREAM_OFF: u16 = 0xFF32;

// Undocumented / developer-only
pub const CMD_LOADSIDCRT: u16 = 0xFF71;
pub const CMD_LOADBOOTCRT: u16 = 0xFF72;
pub const CMD_READFLASH: u16 = 0xFF75;
pub const CMD_DEBUG_REG: u16 = 0xFF76;

// ─────────────────────────────────────────────────────────────────
//  Wire framing
// ─────────────────────────────────────────────────────────────────

/// Build one command frame: `cmd (2B LE) | len (2B LE) | payload`.
///
/// `len_field` is passed separately rather than derived from `payload.len()`
/// because the protocol does not always use it as a byte count — `CMD_WAIT`
/// encodes a millisecond delay there and sends no payload at all. Keeping this
/// a pure function (rather than inlining it into the socket writers) is what
/// makes the framing testable without a device or a TCP listener.
fn encode_frame(cmd: u16, len_field: u16, payload: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(4 + payload.len());
    pkt.extend_from_slice(&cmd.to_le_bytes());
    pkt.extend_from_slice(&len_field.to_le_bytes());
    pkt.extend_from_slice(payload);
    pkt
}

/// Build an image-command frame: `cmd (2B LE) | len_lo (2B LE) | len_hi (1B)`
/// followed by the payload.
///
/// The three image commands (`CMD_MOUNT_IMG`, `CMD_RUN_IMG`, `CMD_RUN_CRT`)
/// carry payloads larger than 64 KB, so the server reads a **24-bit** length:
/// two bytes little-endian, then one more high byte. Getting this wrong
/// truncates or corrupts a whole disk image, which is exactly why it is worth
/// testing as a pure function.
fn encode_image_frame(cmd: u16, data: &[u8]) -> Vec<u8> {
    let len = data.len();
    let len_lo = (len & 0xFFFF) as u16;
    let len_hi = ((len >> 16) & 0xFF) as u8;

    let mut pkt = Vec::with_capacity(5 + data.len());
    pkt.extend_from_slice(&cmd.to_le_bytes());
    pkt.extend_from_slice(&len_lo.to_le_bytes());
    pkt.push(len_hi);
    pkt.extend_from_slice(data);
    pkt
}

// ─────────────────────────────────────────────────────────────────
//  Error type
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Port64Error {
    Connect(String),
    AuthFailed,
    Send(String),
    Recv(String),
    Timeout,
    InvalidResponse(String),
}

impl std::fmt::Display for Port64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Port64Error::Connect(e) => write!(f, "Connect failed: {}", e),
            Port64Error::AuthFailed => write!(f, "Authentication rejected — check password"),
            Port64Error::Send(e) => write!(f, "Send failed: {}", e),
            Port64Error::Recv(e) => write!(f, "Receive failed: {}", e),
            Port64Error::Timeout => write!(f, "Timed out — device may be offline"),
            Port64Error::InvalidResponse(e) => write!(f, "Invalid response: {}", e),
        }
    }
}

impl From<Port64Error> for String {
    fn from(e: Port64Error) -> String {
        e.to_string()
    }
}

pub type Port64Result<T> = Result<T, Port64Error>;

// ─────────────────────────────────────────────────────────────────
//  Flash page info returned by identify_flash()
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FlashGeometry {
    pub page_size: u32,
    pub page_count: u32,
}

impl FlashGeometry {
    pub fn total_bytes(&self) -> u64 {
        self.page_size as u64 * self.page_count as u64
    }
}

// ─────────────────────────────────────────────────────────────────
//  Stream kind (for start/stop streaming helpers)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Vic,
    Audio,
    Debug,
}

// ─────────────────────────────────────────────────────────────────
//  Client
// ─────────────────────────────────────────────────────────────────

/// Async client for the Ultimate 64 raw TCP port-64 protocol.
///
/// Cheap to clone — `host` and `password` are `Arc`-backed strings internally.
#[derive(Debug, Clone)]
pub struct Port64Client {
    host: String,
    password: Option<String>,
    port: u16,
    timeout: std::time::Duration,
    image_timeout: std::time::Duration,
}

impl Port64Client {
    /// Create a new client.  `host` is an IP address or hostname (no port).
    pub fn new(host: impl Into<String>, password: Option<String>) -> Self {
        Self {
            host: host.into(),
            password,
            port: PORT,
            timeout: std::time::Duration::from_secs(TIMEOUT_SECS),
            image_timeout: std::time::Duration::from_secs(IMAGE_TIMEOUT_SECS),
        }
    }

    /// Point the client at a non-default TCP port. The device always uses
    /// [`PORT`]; this exists so tests can drive the client against a local
    /// stub listener (port 64 is privileged and can't be bound in a test).
    #[cfg(test)]
    fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Override the default per-operation timeout.
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout = std::time::Duration::from_secs(secs);
        self
    }

    /// Override the bulk-image transfer budget (see [`IMAGE_TIMEOUT_SECS`]).
    /// Exists so the stalled-transfer guard can be exercised in a test without
    /// waiting out the full production budget.
    pub fn with_image_timeout(mut self, secs: u64) -> Self {
        self.image_timeout = std::time::Duration::from_secs(secs);
        self
    }

    // ── Low-level session plumbing ────────────────────────────────

    /// Open a TCP connection and authenticate if a password is set.
    /// Returns the authenticated stream ready to receive commands.
    async fn open(&self) -> Port64Result<TcpStream> {
        let addr = format!("{}:{}", self.host, self.port);

        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| Port64Error::Timeout)?
            .map_err(|e| Port64Error::Connect(e.to_string()))?;

        // Disable Nagle — we send small command packets and want them out fast
        let _ = stream.set_nodelay(true);

        let mut stream = stream;
        self.authenticate_if_needed(&mut stream).await?;
        Ok(stream)
    }

    /// Send AUTHENTICATE if a non-empty password is configured.
    async fn authenticate_if_needed(&self, stream: &mut TcpStream) -> Port64Result<()> {
        let pw = match &self.password {
            Some(p) if !p.is_empty() => p.as_bytes(),
            _ => return Ok(()),
        };

        let pw_len = pw.len() as u16;
        let mut pkt = Vec::with_capacity(4 + pw.len());
        pkt.extend_from_slice(&CMD_AUTHENTICATE.to_le_bytes());
        pkt.extend_from_slice(&pw_len.to_le_bytes());
        pkt.extend_from_slice(pw);

        stream
            .write_all(&pkt)
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;

        let mut resp = [0u8; 1];
        stream
            .read_exact(&mut resp)
            .await
            .map_err(|e| Port64Error::Recv(e.to_string()))?;

        if resp[0] != 1 {
            return Err(Port64Error::AuthFailed);
        }
        Ok(())
    }

    /// Send a command packet with a raw payload (no extra framing).
    /// `payload` is the complete byte sequence after the 4-byte header.
    async fn send_cmd(&self, stream: &mut TcpStream, cmd: u16, payload: &[u8]) -> Port64Result<()> {
        let pkt = encode_frame(cmd, payload.len() as u16, payload);
        stream
            .write_all(&pkt)
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;
        Ok(())
    }

    /// Send a command where the `len` field carries a value that is NOT the
    /// byte-length of the following payload.  Used by `CMD_WAIT` where `len`
    /// encodes the delay in milliseconds and no payload bytes follow.
    async fn send_cmd_raw_len(
        &self,
        stream: &mut TcpStream,
        cmd: u16,
        len_field: u16,
        payload: &[u8],
    ) -> Port64Result<()> {
        let pkt = encode_frame(cmd, len_field, payload);
        stream
            .write_all(&pkt)
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;
        stream
            .flush()
            .await
            .map_err(|e| Port64Error::Send(e.to_string()))?;
        Ok(())
    }

    /// Read exactly `n` bytes from the stream, with the client timeout.
    async fn recv_exact(&self, stream: &mut TcpStream, n: usize) -> Port64Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        tokio::time::timeout(self.timeout, stream.read_exact(&mut buf))
            .await
            .map_err(|_| Port64Error::Timeout)?
            .map_err(|e| Port64Error::Recv(e.to_string()))?;
        Ok(buf)
    }

    /// Read until EOF or timeout (for variable-length replies like flash pages).
    async fn recv_until_eof(&self, stream: &mut TcpStream) -> Port64Result<Vec<u8>> {
        let mut data = Vec::new();
        let mut chunk = [0u8; 512];
        loop {
            match tokio::time::timeout(self.timeout, stream.read(&mut chunk)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => data.extend_from_slice(&chunk[..n]),
                Ok(Err(e)) => return Err(Port64Error::Recv(e.to_string())),
            }
        }
        Ok(data)
    }

    // ── Public command API ────────────────────────────────────────
    //
    // Every method opens a fresh session, sends the command, reads any
    // reply, and closes.  This matches the C++ server behaviour exactly.

    // ── DMA / execution commands ──────────────────────────────────

    /// `CMD_DMA` — load `data` into C64 RAM via DMA (no auto-run).
    pub async fn dma_load(&self, data: &[u8]) -> Port64Result<()> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_DMA, data).await
    }

    /// `CMD_DMARUN` — load `data` into C64 RAM and run it.
    pub async fn dma_run(&self, data: &[u8]) -> Port64Result<()> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_DMARUN, data).await
    }

    /// `CMD_DMAWRITE` — write `data` to C64 RAM at `address`.
    ///
    /// Payload: `[addr_lo, addr_hi, data…]`
    pub async fn dma_write(&self, address: u16, data: &[u8]) -> Port64Result<()> {
        let mut payload = Vec::with_capacity(2 + data.len());
        payload.extend_from_slice(&address.to_le_bytes());
        payload.extend_from_slice(data);
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_DMAWRITE, &payload).await
    }

    /// `CMD_DMAJUMP` — load `data` into C64 RAM and jump to `address`.
    pub async fn dma_jump(&self, address: u16, data: &[u8]) -> Port64Result<()> {
        let mut payload = Vec::with_capacity(2 + data.len());
        payload.extend_from_slice(&address.to_le_bytes());
        payload.extend_from_slice(data);
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_DMAJUMP, &payload).await
    }

    // ── Machine control ───────────────────────────────────────────

    /// `CMD_RESET` — soft-reset the C64.
    pub async fn reset(&self) -> Port64Result<()> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_RESET, &[]).await
    }

    /// `CMD_POWEROFF` — power off the C64.
    pub async fn poweroff(&self) -> Port64Result<()> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_POWEROFF, &[]).await
    }

    /// `CMD_WAIT` — ask the device to pause for `millis` milliseconds.
    ///
    /// The C++ server calls `vTaskDelay(len)` where `len` is the 16-bit
    /// payload length field — so we encode the delay as the length with an
    /// empty payload.
    pub async fn wait(&self, millis: u16) -> Port64Result<()> {
        // CMD_WAIT is special: the C++ server calls vTaskDelay(len) where
        // `len` is the 2-byte header length field — no payload bytes follow.
        // We use send_cmd_raw_len to put the delay value into the len field.
        let mut s = self.open().await?;
        self.send_cmd_raw_len(&mut s, CMD_WAIT, millis, &[]).await
    }

    // ── Keyboard injection ────────────────────────────────────────

    /// `CMD_KEYB` — inject `text` into the C64 keyboard buffer (`$0277`).
    ///
    /// Maximum 10 characters (C64 keyboard buffer size).  The device also
    /// updates `$00C6` (pending key count) automatically.
    pub async fn type_keys(&self, text: &str) -> Port64Result<()> {
        let bytes: Vec<u8> = text.bytes().take(10).collect();
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_KEYB, &bytes).await
    }

    // ── Memory / ROM operations ───────────────────────────────────

    /// `CMD_REUWRITE` — write `data` into REU expansion RAM at a 24-bit `offset`.
    ///
    /// Payload: `[offset_lo, offset_mid, offset_hi, data…]`
    pub async fn reu_write(&self, offset: u32, data: &[u8]) -> Port64Result<()> {
        let mut payload = Vec::with_capacity(3 + data.len());
        payload.push((offset & 0xFF) as u8);
        payload.push(((offset >> 8) & 0xFF) as u8);
        payload.push(((offset >> 16) & 0xFF) as u8);
        payload.extend_from_slice(data);
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_REUWRITE, &payload).await
    }

    /// `CMD_KERNALWRITE` — replace the active Kernal ROM with `rom_data`.
    ///
    /// The C++ server does `enable_kernal(buf + 2)`, skipping the first two
    /// bytes of the payload, so we prepend two zero bytes automatically.
    /// `rom_data` should be exactly 8 KB (0x2000 bytes).
    pub async fn kernal_write(&self, rom_data: &[u8]) -> Port64Result<()> {
        let mut payload = vec![0u8, 0u8];
        payload.extend_from_slice(rom_data);
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_KERNALWRITE, &payload).await
    }

    // ── Disk / cartridge image commands ───────────────────────────

    /// `CMD_MOUNT_IMG` — upload a disk image and mount it on drive A.
    ///
    /// The device saves the image as `/temp/tcpimage.d64` then mounts it.
    pub async fn mount_image(&self, image_data: &[u8]) -> Port64Result<()> {
        // MOUNT_IMG uses a 3-byte length field: the C++ reads an extra byte
        // after the standard 2-byte len to form a 24-bit value.
        self.send_image_cmd(CMD_MOUNT_IMG, image_data).await
    }

    /// `CMD_RUN_IMG` — upload a disk image, mount it, and run `*` from drive A.
    pub async fn run_image(&self, image_data: &[u8]) -> Port64Result<()> {
        self.send_image_cmd(CMD_RUN_IMG, image_data).await
    }

    /// `CMD_RUN_CRT` — upload a cartridge image and start it.
    ///
    /// The device saves it as `/temp/tcpimage.crt` then calls `FileTypeCRT::execute_st`.
    pub async fn run_crt(&self, crt_data: &[u8]) -> Port64Result<()> {
        self.send_image_cmd(CMD_RUN_CRT, crt_data).await
    }

    /// Internal helper for the three commands that use a 24-bit length field.
    ///
    /// The C++ server reads 2 bytes for the length then reads one extra byte
    /// to form a 24-bit total length for these larger payloads.
    /// A disk image is 175 KB (d64) to ~800 KB (d81), so the write needs its own
    /// budget — but it does need *a* budget: an unbounded `write_all` blocks
    /// forever if the device stops draining the socket (TCP backpressure while
    /// the firmware is busy), and this runs before any caller-side guard.
    async fn send_image_cmd(&self, cmd: u16, data: &[u8]) -> Port64Result<()> {
        let pkt = encode_image_frame(cmd, data);
        let mut s = self.open().await?;
        tokio::time::timeout(self.image_timeout, async {
            s.write_all(&pkt).await?;
            s.flush().await
        })
        .await
        .map_err(|_| Port64Error::Timeout)?
        .map_err(|e| Port64Error::Send(e.to_string()))?;
        Ok(())
    }

    // ── Developer / undocumented ──────────────────────────────────

    /// `CMD_LOADSIDCRT` — load up to 8 KB into the SID cartridge ROM buffer.
    pub async fn load_sid_crt(&self, data: &[u8]) -> Port64Result<()> {
        let trimmed = if data.len() > 0x2000 {
            &data[..0x2000]
        } else {
            data
        };
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_LOADSIDCRT, trimmed).await
    }

    /// `CMD_LOADBOOTCRT` — load up to 8 KB into the boot cartridge ROM buffer.
    pub async fn load_boot_crt(&self, data: &[u8]) -> Port64Result<()> {
        let trimmed = if data.len() > 0x2000 {
            &data[..0x2000]
        } else {
            data
        };
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_LOADBOOTCRT, trimmed).await
    }

    // ── Device identity ───────────────────────────────────────────

    /// `CMD_IDENTIFY` — query the device product title string.
    ///
    /// Returns the identity string, e.g. `"Ultimate 64 Elite"`.
    /// The reply is a Pascal-style string: `[len_byte, chars…]`.
    pub async fn identify(&self) -> Port64Result<String> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_IDENTIFY, &[]).await?;

        // Read 1-byte length prefix
        let len_buf = self.recv_exact(&mut s, 1).await?;
        let len = len_buf[0] as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let chars = self.recv_exact(&mut s, len).await?;
        String::from_utf8(chars)
            .map_err(|e| Port64Error::InvalidResponse(format!("Non-UTF8 identity: {}", e)))
    }

    // ── Flash memory ──────────────────────────────────────────────

    /// `CMD_READFLASH` sub-cmd 0+1 — query flash geometry (page size + count).
    pub async fn flash_geometry(&self) -> Port64Result<FlashGeometry> {
        let page_size = self.flash_query(0).await?;
        let page_count = self.flash_query(1).await?;
        Ok(FlashGeometry {
            page_size,
            page_count,
        })
    }

    /// `CMD_READFLASH` sub-cmd 2 — read one flash page by index.
    pub async fn flash_read_page(&self, page: u32) -> Port64Result<Vec<u8>> {
        let mut payload = [0u8; 4];
        payload[0] = 2; // sub-command: get page
        payload[1] = (page & 0xFF) as u8;
        payload[2] = ((page >> 8) & 0xFF) as u8;
        payload[3] = ((page >> 16) & 0xFF) as u8;

        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_READFLASH, &payload).await?;
        let data = self.recv_until_eof(&mut s).await?;

        if data.is_empty() {
            return Err(Port64Error::InvalidResponse(format!(
                "Flash page {} returned no data",
                page
            )));
        }
        Ok(data)
    }

    /// Internal: `CMD_READFLASH` sub-cmd 0 or 1 → returns a `u32`.
    async fn flash_query(&self, sub_cmd: u8) -> Port64Result<u32> {
        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_READFLASH, &[sub_cmd]).await?;
        let buf = self.recv_exact(&mut s, 4).await?;
        Ok(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
    }

    // ── Debug register (U64 only) ─────────────────────────────────

    /// `CMD_DEBUG_REG` — read (and optionally write) the U64 debug register.
    ///
    /// Always returns the current register value.  If `write` is `Some(v)`
    /// the register is updated to `v` after the read.
    pub async fn debug_register(&self, write: Option<u8>) -> Port64Result<u8> {
        let owned;
        let payload: &[u8] = match write {
            Some(v) => {
                owned = [v];
                &owned
            }
            None => &[],
        };

        let mut s = self.open().await?;
        self.send_cmd(&mut s, CMD_DEBUG_REG, payload).await?;
        let buf = self.recv_exact(&mut s, 1).await?;
        Ok(buf[0])
    }

    // ── Streaming (U64 only) ──────────────────────────────────────

    /// `CMD_VICSTREAM_ON` / `CMD_AUDIOSTREAM_ON` / `CMD_DEBUGSTREAM_ON`
    ///
    /// Start a stream of the given `kind`.
    /// `buffer_size`: optional frame/buffer size hint (0 = device default).
    /// `name`: optional stream name / destination hint (may be empty).
    ///
    /// Note: starting VIC stream automatically stops DEBUG stream, and
    /// vice-versa, mirroring the C++ logic.
    pub async fn stream_start(
        &self,
        kind: StreamKind,
        buffer_size: u16,
        name: &str,
    ) -> Port64Result<()> {
        let cmd = match kind {
            StreamKind::Vic => CMD_VICSTREAM_ON,
            StreamKind::Audio => CMD_AUDIOSTREAM_ON,
            StreamKind::Debug => CMD_DEBUGSTREAM_ON,
        };
        // Payload: [buf_lo, buf_hi, name_bytes…]
        let mut payload = Vec::with_capacity(2 + name.len());
        payload.extend_from_slice(&buffer_size.to_le_bytes());
        payload.extend_from_slice(name.as_bytes());

        let mut s = self.open().await?;
        self.send_cmd(&mut s, cmd, &payload).await
    }

    /// `CMD_VICSTREAM_OFF` / `CMD_AUDIOSTREAM_OFF` / `CMD_DEBUGSTREAM_OFF`
    pub async fn stream_stop(&self, kind: StreamKind) -> Port64Result<()> {
        let cmd = match kind {
            StreamKind::Vic => CMD_VICSTREAM_OFF,
            StreamKind::Audio => CMD_AUDIOSTREAM_OFF,
            StreamKind::Debug => CMD_DEBUGSTREAM_OFF,
        };
        let mut s = self.open().await?;
        self.send_cmd(&mut s, cmd, &[]).await
    }
}

// ─────────────────────────────────────────────────────────────────
//  Convenience free functions
//
//  These are thin wrappers around Port64Client for callers that want
//  a fire-and-forget style without constructing a client explicitly.
//  Used by memory_editor.rs via `port64::write_dma(...)`.
// ─────────────────────────────────────────────────────────────────

/// Fire-and-forget `dma_write`.
pub async fn write_dma(
    host: String,
    password: Option<String>,
    address: u16,
    data: Vec<u8>,
) -> Result<(), String> {
    Port64Client::new(host, password)
        .dma_write(address, &data)
        .await
        .map_err(|e| e.to_string())
}

/// Fire-and-forget `CMD_RUN_IMG` — upload a disk image and have the *firmware*
/// mount it and run `*` from drive A. This is the device's own "run disk"
/// (no client-side `LOAD"*"`/keyboard/DMA), so it's the most reliable boot for
/// `.d64` images. The firmware always stores the payload as `tcpimage.d64`, so
/// this is d64-only — callers use the REST mount+boot fallback for other formats
/// and when port 64 (the Ultimate DMA service) is unavailable.
pub async fn run_disk_image(
    host: String,
    password: Option<String>,
    image: Vec<u8>,
) -> Result<(), String> {
    Port64Client::new(host, password)
        .run_image(&image)
        .await
        .map_err(|e| e.to_string())
}

/// Fire-and-forget `dma_jump`.
pub async fn write_dma_jump(
    host: String,
    password: Option<String>,
    address: u16,
    data: Vec<u8>,
) -> Result<(), String> {
    Port64Client::new(host, password)
        .dma_jump(address, &data)
        .await
        .map_err(|e| e.to_string())
}

/// Fire-and-forget `reu_write`.
pub async fn write_reu(
    host: String,
    password: Option<String>,
    offset: u32,
    data: Vec<u8>,
) -> Result<(), String> {
    Port64Client::new(host, password)
        .reu_write(offset, &data)
        .await
        .map_err(|e| e.to_string())
}

/// Fire-and-forget `kernal_write`.
pub async fn write_kernal(
    host: String,
    password: Option<String>,
    rom_data: Vec<u8>,
) -> Result<(), String> {
    Port64Client::new(host, password)
        .kernal_write(&rom_data)
        .await
        .map_err(|e| e.to_string())
}

/// Query flash geometry (page_size, page_count).
pub async fn flash_info(host: String, password: Option<String>) -> Result<(u32, u32), String> {
    Port64Client::new(host, password)
        .flash_geometry()
        .await
        .map(|g| (g.page_size, g.page_count))
        .map_err(|e| e.to_string())
}

/// Read one flash page by index.
pub async fn flash_page(
    host: String,
    password: Option<String>,
    page: u32,
) -> Result<(u32, Vec<u8>), String> {
    Port64Client::new(host, password)
        .flash_read_page(page)
        .await
        .map(|data| (page, data))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_is_cmd_then_len_little_endian() {
        let f = encode_frame(CMD_RESET, 0, &[]);
        // 0xFF04 little-endian = 04 FF, then a zero length.
        assert_eq!(f, vec![0x04, 0xFF, 0x00, 0x00]);
    }

    #[test]
    fn frame_carries_payload_after_the_4_byte_header() {
        let f = encode_frame(CMD_DMAWRITE, 3, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(&f[..2], &[0x06, 0xFF]); // cmd 0xFF06
        assert_eq!(&f[2..4], &[0x03, 0x00]); // len = 3
        assert_eq!(&f[4..], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(f.len(), 4 + 3);
    }

    /// `CMD_WAIT` puts a millisecond delay in the length field and sends no
    /// payload — the reason the length is a separate parameter.
    #[test]
    fn raw_len_field_is_independent_of_payload_length() {
        let f = encode_frame(CMD_WAIT, 500, &[]);
        assert_eq!(&f[2..4], &[0xF4, 0x01]); // 500 = 0x01F4, little-endian
        assert_eq!(f.len(), 4, "no payload bytes follow CMD_WAIT");
    }

    #[test]
    fn image_frame_uses_a_5_byte_24bit_header() {
        let data = vec![0u8; 10];
        let f = encode_image_frame(CMD_RUN_IMG, &data);
        assert_eq!(&f[..2], &[0x0B, 0xFF]); // cmd 0xFF0B
        assert_eq!(&f[2..4], &[0x0A, 0x00]); // len_lo = 10
        assert_eq!(f[4], 0x00); // len_hi
        assert_eq!(f.len(), 5 + 10);
    }

    /// A standard d64 is 174848 bytes — past 64 KB, so it exercises the high
    /// byte. Getting this wrong truncates the image and the disk won't boot.
    #[test]
    fn image_frame_encodes_a_real_d64_length_across_the_16bit_boundary() {
        const D64_LEN: usize = 174_848; // 0x02AB00
        let f = encode_image_frame(CMD_RUN_IMG, &vec![0u8; D64_LEN]);
        assert_eq!(&f[2..4], &[0x00, 0xAB], "low 16 bits of 0x02AB00");
        assert_eq!(f[4], 0x02, "high byte must carry bits 16..24");
        assert_eq!(f.len(), 5 + D64_LEN);

        // Round-trip: decode the way the firmware does.
        let decoded = u32::from(f[2]) | (u32::from(f[3]) << 8) | (u32::from(f[4]) << 16);
        assert_eq!(decoded as usize, D64_LEN);
    }

    /// The stalled-transfer guard: a peer that accepts the connection but never
    /// reads. Once the kernel socket buffers fill, `write_all` blocks forever —
    /// which is exactly the hang this timeout was added to bound. Uses a 1s
    /// override so the test doesn't wait out the 60s production budget.
    #[tokio::test]
    async fn image_send_times_out_when_the_peer_stops_reading() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Accept, then hold the connection open without ever reading from it.
        let _accepter = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(stream);
        });

        // Payload far larger than any plausible socket buffer, so the write
        // cannot drain into the kernel and must block.
        let big = vec![0u8; 16 * 1024 * 1024];
        let client = Port64Client::new("127.0.0.1", None)
            .with_port(port)
            .with_image_timeout(1);

        let started = std::time::Instant::now();
        let err = client
            .send_image_cmd(CMD_RUN_IMG, &big)
            .await
            .expect_err("a non-reading peer must not look like success");

        assert!(
            matches!(err, Port64Error::Timeout),
            "expected Timeout, got {err:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "guard should fire at ~1s, took {:?}",
            started.elapsed()
        );
    }

    /// A d81 (~800 KB) still fits the 24-bit field with room to spare.
    #[test]
    fn image_frame_handles_a_d81_sized_payload() {
        const D81_LEN: usize = 819_200; // 0x0C8000
        let f = encode_image_frame(CMD_MOUNT_IMG, &vec![0u8; D81_LEN]);
        let decoded = u32::from(f[2]) | (u32::from(f[3]) << 8) | (u32::from(f[4]) << 16);
        assert_eq!(decoded as usize, D81_LEN);
    }
}
