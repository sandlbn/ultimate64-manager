/// Supports all C64 graphics modes including cases where graphics data lives under
/// Kernal/BASIC ROM (VIC bank 3 at $E000-$FFFF).  In those cases an NMI-based ROM
/// bypass technique is used: a tiny 6502 copy-stub is injected, triggered via CIA2
/// Timer A, and the data is copied to a safe RAM buffer before being read back.
///
/// Based on the C64U-Screenshot Python tool by Garland Glessner (GPL-3.0).
use crate::video_scaling::C64_PALETTE;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── constants ──────────────────────────────────────────────────────────────────

/// Cassette buffer – safe scratch area for our injected 6502 stub
const STUB_ADDR: u16 = 0x0340;

/// Where the stub copies ROM-shadowed data (8 KB inside VIC bank 1 – rarely used)
const COPY_BUFFER: u16 = 0x4000;

// ── REST client ───────────────────────────────────────────────────────────────

/// Thin wrapper around the Ultimate64 REST API.
/// Uses the *blocking* reqwest client so this can be called from `spawn_blocking`.
struct U64Api {
    base: String,
    password: Option<String>,
}

impl U64Api {
    fn new(host: &str, password: Option<String>) -> Self {
        Self {
            base: format!("http://{}", host),
            password,
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|e| format!("HTTP client build failed: {}", e))
    }

    fn extra_headers(&self) -> reqwest::header::HeaderMap {
        let mut m = reqwest::header::HeaderMap::new();
        if let Some(pw) = &self.password {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(pw) {
                m.insert("X-Password", v);
            }
        }
        m
    }

    fn pause(&self) -> Result<(), String> {
        let client = self.client()?;
        client
            .put(format!("{}/v1/machine:pause", self.base))
            .headers(self.extra_headers())
            .send()
            .map_err(|e| format!("pause failed: {}", e))?;
        Ok(())
    }

    fn resume(&self) -> Result<(), String> {
        let client = self.client()?;
        client
            .put(format!("{}/v1/machine:resume", self.base))
            .headers(self.extra_headers())
            .send()
            .map_err(|e| format!("resume failed: {}", e))?;
        Ok(())
    }

    fn read_mem(&self, addr: u16, len: usize) -> Result<Vec<u8>, String> {
        let client = self.client()?;
        let url = format!("{}/v1/machine:readmem", self.base);
        let resp = client
            .get(&url)
            .headers(self.extra_headers())
            .query(&[
                ("address", format!("{:X}", addr)),
                ("length", len.to_string()),
            ])
            .send()
            .map_err(|e| format!("readmem failed: {}", e))?;
        if resp.status().is_success() {
            Ok(resp.bytes().map_err(|e| e.to_string())?.to_vec())
        } else {
            Err(format!("readmem HTTP {}", resp.status()))
        }
    }

    fn write_mem(&self, addr: u16, data: &[u8]) -> Result<(), String> {
        let client = self.client()?;
        let url = format!("{}/v1/machine:writemem", self.base);
        let resp = client
            .post(&url)
            .headers(self.extra_headers())
            .query(&[("address", format!("{:X}", addr))])
            .header("Content-Type", "application/octet-stream")
            .body(data.to_vec())
            .send()
            .map_err(|e| format!("writemem failed: {}", e))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("writemem HTTP {}", resp.status()))
        }
    }
}

// ── VIC-II state ──────────────────────────────────────────────────────────────

struct VicState {
    // mode bits
    bmm: bool,
    ecm: bool,
    mcm: bool,
    den: bool,
    rsel: bool,
    csel: bool,
    yscroll: u8,
    xscroll: u8,
    // addresses
    vic_bank: u16,
    screen_mem_addr: u16,
    char_mem_addr: u16,
    bitmap_mem_addr: u16,
    char_mem_offset: u16,
    // colors
    border_color: usize,
    background_color: usize,
    background_color1: usize,
    background_color2: usize,
    background_color3: usize,
    sprite_multicolor0: usize,
    sprite_multicolor1: usize,
    // raw registers (for sprites)
    raw: Vec<u8>,
}

impl VicState {
    fn from_regs(vic: &[u8], cia2_pa: u8) -> Self {
        let d011 = vic[0x11];
        let d016 = vic[0x16];
        let d018 = vic[0x18];

        let char_mem_offset = (((d018 >> 1) & 0x07) as u16) * 0x0800;
        let screen_mem_offset = (((d018 >> 4) & 0x0F) as u16) * 0x0400;
        let bitmap_mem_offset = (((d018 >> 3) & 1) as u16) * 0x2000;

        let vic_bank = (3 - (cia2_pa & 0x03)) as u16 * 0x4000;

        VicState {
            bmm: (d011 >> 5) & 1 == 1,
            ecm: (d011 >> 6) & 1 == 1,
            mcm: (d016 >> 4) & 1 == 1,
            den: (d011 >> 4) & 1 == 1,
            rsel: (d011 >> 3) & 1 == 1,
            csel: (d016 >> 3) & 1 == 1,
            yscroll: d011 & 0x07,
            xscroll: d016 & 0x07,
            vic_bank,
            screen_mem_addr: vic_bank + screen_mem_offset,
            char_mem_addr: vic_bank + char_mem_offset,
            bitmap_mem_addr: vic_bank + bitmap_mem_offset,
            char_mem_offset,
            border_color: (vic[0x20] & 0x0F) as usize,
            background_color: (vic[0x21] & 0x0F) as usize,
            background_color1: (vic[0x22] & 0x0F) as usize,
            background_color2: (vic[0x23] & 0x0F) as usize,
            background_color3: (vic[0x24] & 0x0F) as usize,
            sprite_multicolor0: (vic[0x25] & 0x0F) as usize,
            sprite_multicolor1: (vic[0x26] & 0x0F) as usize,
            raw: vic.to_vec(),
        }
    }

    fn mode_name(&self) -> &'static str {
        match (self.ecm, self.bmm, self.mcm) {
            (true, false, false) => "Extended Background Color",
            (false, true, false) => "Standard Bitmap (Hi-Res)",
            (false, true, true) => "Multicolor Bitmap",
            (false, false, true) => "Multicolor Text",
            (false, false, false) => "Standard Text",
            _ => "Invalid/Unused",
        }
    }
}

// ── ROM-bypass: inject 6502 stub via NMI ─────────────────────────────────────

/// Returns true if [addr, addr+len) overlaps with Kernal ($E000) or BASIC ($A000) ROM.
fn overlaps_rom(addr: u16, len: usize) -> bool {
    let end = addr as usize + len;
    (addr as usize <= 0xFFFF && end > 0xE000) || (addr as usize <= 0xBFFF && end > 0xA000)
}

/// Build 6502 machine code that copies `length` bytes from `src` to `dst`
/// with all ROMs banked out, then jumps to `original_nmi` when done.
fn build_copy_stub(src: u16, dst: u16, length: usize, original_nmi: u16) -> Vec<u8> {
    let mut c: Vec<u8> = Vec::with_capacity(80);

    // Save A/X/Y on stack
    c.extend_from_slice(&[0x48, 0x8A, 0x48, 0x98, 0x48]);
    // Save $01, then bank out all ROMs: $01 = $34
    c.extend_from_slice(&[0xA5, 0x01, 0x48, 0xA9, 0x34, 0x85, 0x01]);
    // $FB/$FC = src,  $FD/$FE = dst
    c.extend_from_slice(&[0xA9, (src & 0xFF) as u8, 0x85, 0xFB]);
    c.extend_from_slice(&[0xA9, (src >> 8) as u8, 0x85, 0xFC]);
    c.extend_from_slice(&[0xA9, (dst & 0xFF) as u8, 0x85, 0xFD]);
    c.extend_from_slice(&[0xA9, (dst >> 8) as u8, 0x85, 0xFE]);

    let num_pages = ((length + 255) / 256) as u8;
    // LDX #num_pages
    c.extend_from_slice(&[0xA2, num_pages]);
    // outer: LDY #0  inner: LDA ($FB),Y / STA ($FD),Y / INY / BNE inner
    c.extend_from_slice(&[0xA0, 0x00, 0xB1, 0xFB, 0x91, 0xFD, 0xC8, 0xD0, 0xF9]);
    // INC $FC / INC $FE / DEX / BNE outer
    c.extend_from_slice(&[0xE6, 0xFC, 0xE6, 0xFE, 0xCA, 0xD0, 0xF0]);
    // Restore $01
    c.extend_from_slice(&[0x68, 0x85, 0x01]);
    // Write completion marker $42 to $02
    c.extend_from_slice(&[0xA9, 0x42, 0x85, 0x02]);
    // Restore Y/X/A
    c.extend_from_slice(&[0x68, 0xA8, 0x68, 0xAA, 0x68]);
    // JMP original_nmi
    c.extend_from_slice(&[0x4C, (original_nmi & 0xFF) as u8, (original_nmi >> 8) as u8]);

    c
}

/// Read memory that lives under a ROM by injecting and running a copy stub.
///
/// Saves all modified locations, injects stub + NMI vector redirect,
/// triggers CIA2 Timer A, waits, reads copied data, then restores everything.
fn read_via_rom_bypass(api: &U64Api, src: u16, length: usize) -> Result<Vec<u8>, String> {
    log::info!(
        "screenshot_api: ROM bypass ${:04X}-${:04X} → buffer ${:04X}",
        src,
        src as usize + length - 1,
        COPY_BUFFER
    );

    // ── back up everything we'll touch ────────────────────────────────────────
    let stub_backup = api.read_mem(STUB_ADDR, 128)?;
    let buf_backup = api.read_mem(COPY_BUFFER, length)?;
    let zp_backup = api.read_mem(0x00FB, 4)?;
    let marker_backup = api.read_mem(0x0002, 1)?;
    let nmi_vec_backup = api.read_mem(0x0318, 2)?;
    let _cia2_icr_backup = api.read_mem(0xDD0D, 1)?;
    let cia2_tmr_backup = api.read_mem(0xDD04, 3)?;

    let original_nmi = nmi_vec_backup[0] as u16 | ((nmi_vec_backup[1] as u16) << 8);
    log::debug!(
        "screenshot_api: original NMI handler = ${:04X}",
        original_nmi
    );

    let result = (|| -> Result<Vec<u8>, String> {
        // ── inject stub ───────────────────────────────────────────────────────
        let stub = build_copy_stub(src, COPY_BUFFER, length, original_nmi);
        api.write_mem(STUB_ADDR, &stub)?;

        // Redirect NMI vector to our stub
        api.write_mem(0x0318, &[STUB_ADDR as u8, (STUB_ADDR >> 8) as u8])?;

        // Clear completion marker
        api.write_mem(0x0002, &[0x00])?;

        // ── trigger NMI via CIA2 Timer A ──────────────────────────────────────
        // Acknowledge pending IRQs by reading ICR
        let _ = api.read_mem(0xDD0D, 1);
        api.write_mem(0xDD04, &[0x02, 0x00])?; // Timer = 2 cycles
        api.write_mem(0xDD0D, &[0x81])?; // Enable Timer A NMI (bit7=set, bit0=TimerA)
        api.write_mem(0xDD0E, &[0x11])?; // Start Timer A + force load

        // ── run briefly then re-freeze ────────────────────────────────────────
        api.resume()?;
        std::thread::sleep(Duration::from_millis(600)); // 8 KB @ 1 MHz ≈ 100 ms; 600 ms is safe
        api.pause()?;

        // Verify marker
        let marker = api.read_mem(0x0002, 1)?;
        if marker[0] != 0x42 {
            log::warn!(
                "screenshot_api: ROM bypass marker mismatch (got {:02X}, want 42)",
                marker[0]
            );
        } else {
            log::debug!("screenshot_api: ROM bypass copy confirmed");
        }

        // ── read copied data ──────────────────────────────────────────────────
        api.read_mem(COPY_BUFFER, length)
    })();

    // ── always restore ─────────────────────────────────────────────────────────
    log::debug!("screenshot_api: restoring memory after ROM bypass");
    api.pause().ok(); // make sure we're paused
    api.write_mem(0xDD0D, &[0x01]).ok(); // disable Timer A NMI
    api.write_mem(0xDD04, &cia2_tmr_backup).ok();
    api.write_mem(0x0318, &nmi_vec_backup).ok();
    api.write_mem(0x00FB, &zp_backup).ok();
    api.write_mem(0x0002, &marker_backup).ok();
    api.write_mem(STUB_ADDR, &stub_backup).ok();
    api.write_mem(COPY_BUFFER, &buf_backup).ok();

    result
}

/// Read `length` bytes from `addr`, automatically using ROM bypass when needed.
fn smart_read(api: &U64Api, addr: u16, length: usize) -> Result<Vec<u8>, String> {
    if overlaps_rom(addr, length) {
        log::info!(
            "screenshot_api: ROM overlap at ${:04X} – using bypass",
            addr
        );
        read_via_rom_bypass(api, addr, length)
    } else {
        api.read_mem(addr, length)
    }
}

// ── character ROM (embedded fallback) ─────────────────────────────────────────

/// Returns 2 KB of the standard C64 uppercase/graphics character set.
/// Used when the program points VIC at the built-in character ROM
/// ($D000-$DFFF as seen through VIC bank 0 / bank 2).
fn embedded_char_rom() -> Vec<u8> {
    // Patterns for chars 0-127 (uppercase/graphics set).
    // Each entry is (char_code, [8 bytes]).
    #[rustfmt::skip]
    const PATTERNS: &[(usize, [u8; 8])] = &[
        (0,   [0x3C, 0x66, 0x6E, 0x6E, 0x60, 0x62, 0x3C, 0x00]),
        (1,   [0x18, 0x3C, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00]),
        (2,   [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00]),
        (3,   [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00]),
        (4,   [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00]),
        (5,   [0x7E, 0x60, 0x60, 0x78, 0x60, 0x60, 0x7E, 0x00]),
        (6,   [0x7E, 0x60, 0x60, 0x78, 0x60, 0x60, 0x60, 0x00]),
        (7,   [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00]),
        (8,   [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00]),
        (9,   [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00]),
        (10,  [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x6C, 0x38, 0x00]),
        (11,  [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00]),
        (12,  [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00]),
        (13,  [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00]),
        (14,  [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00]),
        (15,  [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00]),
        (16,  [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00]),
        (17,  [0x3C, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x0E, 0x00]),
        (18,  [0x7C, 0x66, 0x66, 0x7C, 0x78, 0x6C, 0x66, 0x00]),
        (19,  [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00]),
        (20,  [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00]),
        (21,  [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00]),
        (22,  [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00]),
        (23,  [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00]),
        (24,  [0x66, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x66, 0x00]),
        (25,  [0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x00]),
        (26,  [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x7E, 0x00]),
        (27,  [0x3C, 0x30, 0x30, 0x30, 0x30, 0x30, 0x3C, 0x00]),
        (28,  [0x0C, 0x12, 0x30, 0x7C, 0x30, 0x62, 0xFC, 0x00]),
        (29,  [0x3C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x3C, 0x00]),
        (30,  [0x00, 0x08, 0x1C, 0x3E, 0x08, 0x08, 0x00, 0x00]),
        (31,  [0x00, 0x10, 0x30, 0x7F, 0x30, 0x10, 0x00, 0x00]),
        (32,  [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
        (33,  [0x18, 0x18, 0x18, 0x18, 0x00, 0x00, 0x18, 0x00]),
        (34,  [0x66, 0x66, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00]),
        (35,  [0x66, 0x66, 0xFF, 0x66, 0xFF, 0x66, 0x66, 0x00]),
        (36,  [0x18, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x18, 0x00]),
        (37,  [0x62, 0x66, 0x0C, 0x18, 0x30, 0x66, 0x46, 0x00]),
        (38,  [0x3C, 0x66, 0x3C, 0x38, 0x67, 0x66, 0x3F, 0x00]),
        (39,  [0x06, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00]),
        (40,  [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00]),
        (41,  [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00]),
        (42,  [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00]),
        (43,  [0x00, 0x18, 0x18, 0x7E, 0x18, 0x18, 0x00, 0x00]),
        (44,  [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x30]),
        (45,  [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00]),
        (46,  [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00]),
        (47,  [0x00, 0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x00]),
        (48,  [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00]),
        (49,  [0x18, 0x18, 0x38, 0x18, 0x18, 0x18, 0x7E, 0x00]),
        (50,  [0x3C, 0x66, 0x06, 0x0C, 0x30, 0x60, 0x7E, 0x00]),
        (51,  [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00]),
        (52,  [0x06, 0x0E, 0x1E, 0x66, 0x7F, 0x06, 0x06, 0x00]),
        (53,  [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00]),
        (54,  [0x3C, 0x66, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00]),
        (55,  [0x7E, 0x66, 0x0C, 0x18, 0x18, 0x18, 0x18, 0x00]),
        (56,  [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00]),
        (57,  [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x66, 0x3C, 0x00]),
        (58,  [0x00, 0x00, 0x18, 0x00, 0x00, 0x18, 0x00, 0x00]),
        (59,  [0x00, 0x00, 0x18, 0x00, 0x00, 0x18, 0x18, 0x30]),
        (60,  [0x0E, 0x18, 0x30, 0x60, 0x30, 0x18, 0x0E, 0x00]),
        (61,  [0x00, 0x00, 0x7E, 0x00, 0x7E, 0x00, 0x00, 0x00]),
        (62,  [0x70, 0x18, 0x0C, 0x06, 0x0C, 0x18, 0x70, 0x00]),
        (63,  [0x3C, 0x66, 0x06, 0x0C, 0x18, 0x00, 0x18, 0x00]),
        (64,  [0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00]),
        (65,  [0x08, 0x1C, 0x3E, 0x7F, 0x7F, 0x1C, 0x3E, 0x00]),
        (66,  [0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18]),
        (67,  [0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]),
        (68,  [0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00]),
        (69,  [0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0]),
        (70,  [0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA]),
        (71,  [0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F]),
        (72,  [0x00, 0x00, 0x00, 0x00, 0xAA, 0x55, 0xAA, 0x55]),
        (73,  [0x0F, 0x07, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00]),
        (74,  [0x55, 0xAA, 0x55, 0xAA, 0x00, 0x00, 0x00, 0x00]),
        (75,  [0x00, 0x00, 0x00, 0x00, 0x01, 0x03, 0x07, 0x0F]),
        (76,  [0x00, 0x00, 0x00, 0x00, 0x80, 0xC0, 0xE0, 0xF0]),
        (77,  [0xF0, 0xE0, 0xC0, 0x80, 0x00, 0x00, 0x00, 0x00]),
        (78,  [0x01, 0x03, 0x07, 0x0F, 0x1F, 0x3F, 0x7F, 0xFF]),
        (79,  [0x80, 0xC0, 0xE0, 0xF0, 0xF8, 0xFC, 0xFE, 0xFF]),
        (80,  [0xFF, 0xFE, 0xFC, 0xF8, 0xF0, 0xE0, 0xC0, 0x80]),
        (81,  [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
        (82,  [0xFF, 0x7F, 0x3F, 0x1F, 0x0F, 0x07, 0x03, 0x01]),
        (83,  [0x3C, 0x7E, 0xFF, 0xFF, 0xFF, 0xFF, 0x7E, 0x3C]),
        (84,  [0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0, 0xC0]),
        (85,  [0x18, 0x18, 0x7E, 0xFF, 0xFF, 0x18, 0x3C, 0x00]),
        (86,  [0x00, 0x00, 0x00, 0x00, 0xF0, 0xF0, 0xF0, 0xF0]),
        (87,  [0x0F, 0x0F, 0x0F, 0x0F, 0x00, 0x00, 0x00, 0x00]),
        (88,  [0x00, 0x00, 0x00, 0x00, 0x0F, 0x0F, 0x0F, 0x0F]),
        (89,  [0xF8, 0xF0, 0xE0, 0xC0, 0x80, 0x00, 0x00, 0x00]),
        (90,  [0xF0, 0xF0, 0xF0, 0xF0, 0x00, 0x00, 0x00, 0x00]),
        (91,  [0x00, 0x66, 0xFF, 0xFF, 0xFF, 0x7E, 0x3C, 0x18]),
        (92,  [0x00, 0x00, 0x00, 0x80, 0xC0, 0xE0, 0xF0, 0xF8]),
        (93,  [0x18, 0x18, 0x18, 0xFF, 0xFF, 0x18, 0x18, 0x18]),
        (94,  [0x00, 0x3C, 0x42, 0x42, 0x42, 0x42, 0x3C, 0x00]),
        (95,  [0x18, 0x3C, 0x7E, 0xFF, 0x7E, 0x3C, 0x18, 0x00]),
        (96,  [0x00, 0x00, 0x00, 0x01, 0x03, 0x07, 0x0F, 0x1F]),
        (97,  [0x1F, 0x0F, 0x07, 0x03, 0x01, 0x00, 0x00, 0x00]),
        (98,  [0x00, 0x00, 0x7F, 0x36, 0x36, 0x36, 0x63, 0x00]),
        (99,  [0xFF, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]),
        (100, [0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03]),
        (101, [0xC0, 0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01]),
        (102, [0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]),
        (103, [0x01, 0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0xC0]),
        (104, [0x00, 0x00, 0x00, 0x00, 0xC0, 0xC0, 0xC0, 0xC0]),
        (105, [0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00]),
        (106, [0x00, 0x00, 0x00, 0x00, 0x03, 0x03, 0x03, 0x03]),
        (107, [0xC0, 0xC0, 0xC0, 0xC0, 0x00, 0x00, 0x00, 0x00]),
        (108, [0x03, 0x03, 0x03, 0x03, 0x00, 0x00, 0x00, 0x00]),
        (109, [0x00, 0x00, 0x00, 0xFF, 0xFF, 0x18, 0x18, 0x18]),
        (110, [0x18, 0x18, 0x18, 0xFF, 0xFF, 0x00, 0x00, 0x00]),
        (111, [0x18, 0x18, 0x18, 0x1F, 0x1F, 0x18, 0x18, 0x18]),
        (112, [0x18, 0x18, 0x18, 0xF8, 0xF8, 0x00, 0x00, 0x00]),
        (113, [0x00, 0x00, 0x00, 0xF8, 0xF8, 0x18, 0x18, 0x18]),
        (114, [0x00, 0x00, 0x00, 0x1F, 0x1F, 0x18, 0x18, 0x18]),
        (115, [0x18, 0x18, 0x18, 0x1F, 0x1F, 0x00, 0x00, 0x00]),
        (116, [0x18, 0x18, 0x18, 0xF8, 0xF8, 0x18, 0x18, 0x18]),
        (117, [0x18, 0x18, 0x18, 0xFF, 0xFF, 0x18, 0x18, 0x18]),
        (118, [0x3C, 0x3C, 0x3C, 0x3C, 0x3C, 0x3C, 0x3C, 0x3C]),
        (119, [0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00]),
        (120, [0x00, 0x00, 0x00, 0x00, 0x3C, 0x3C, 0x3C, 0x3C]),
        (121, [0x3C, 0x3C, 0x3C, 0x3C, 0x00, 0x00, 0x00, 0x00]),
        (122, [0x00, 0x00, 0x00, 0x00, 0x3C, 0x3C, 0x3C, 0x3C]),
        (123, [0x3C, 0x3C, 0x3C, 0x3C, 0x00, 0x00, 0x00, 0x00]),
        (124, [0x00, 0x00, 0xFC, 0xFC, 0x3C, 0x3C, 0x3C, 0x3C]),
        (125, [0x3C, 0x3C, 0x3C, 0x3C, 0x3F, 0x3F, 0x00, 0x00]),
        (126, [0x00, 0x7E, 0x66, 0x66, 0x66, 0x66, 0x00, 0x00]),
        (127, [0x08, 0x1C, 0x3E, 0x7F, 0x3E, 0x1C, 0x08, 0x00]),
    ];

    let mut rom = vec![0u8; 2048];
    for &(code, ref pat) in PATTERNS {
        if code < 128 {
            rom[code * 8..code * 8 + 8].copy_from_slice(pat);
        }
    }
    // Second half mirrors first (lowercase/graphics alternate set)
    for i in 0..1024 {
        rom[1024 + i] = rom[i];
    }
    rom
}

// ── renderers ─────────────────────────────────────────────────────────────────

fn rgb(idx: usize) -> [u8; 3] {
    C64_PALETTE[idx & 0x0F]
}

fn set_px(buf: &mut [u8], w: usize, x: usize, y: usize, c: [u8; 3]) {
    let base = (y * w + x) * 3;
    buf[base] = c[0];
    buf[base + 1] = c[1];
    buf[base + 2] = c[2];
}

/// Standard text mode  (320×200 RGB)
fn render_text(vic: &VicState, screen: &[u8], color: &[u8], chars: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 320 * 200 * 3];
    let bg = rgb(vic.background_color);
    for row in 0..25usize {
        for col in 0..40usize {
            let pos = row * 40 + col;
            let ch = screen[pos] as usize;
            let fg = rgb((color[pos] & 0x0F) as usize);
            for py in 0..8usize {
                let byte = chars[(ch * 8 + py).min(chars.len() - 1)];
                for px in 0..8usize {
                    let c = if byte & (0x80 >> px) != 0 { fg } else { bg };
                    set_px(&mut buf, 320, col * 8 + px, row * 8 + py, c);
                }
            }
        }
    }
    buf
}

/// Multicolor text mode  (320×200 RGB – multicolor chars are 4px wide)
fn render_mc_text(vic: &VicState, screen: &[u8], color: &[u8], chars: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 320 * 200 * 3];
    let bg0 = rgb(vic.background_color);
    let bg1 = rgb(vic.background_color1);
    let bg2 = rgb(vic.background_color2);
    for row in 0..25usize {
        for col in 0..40usize {
            let pos = row * 40 + col;
            let ch = screen[pos] as usize;
            let color_byte = color[pos];
            let is_mc = color_byte & 0x08 != 0;
            let fg = rgb((color_byte & 0x07) as usize);
            for py in 0..8usize {
                let byte = chars[(ch * 8 + py).min(chars.len() - 1)];
                if is_mc {
                    for hx in 0..4usize {
                        let bits = (byte >> (6 - hx * 2)) & 0x03;
                        let c = match bits {
                            0 => bg0,
                            1 => bg1,
                            2 => bg2,
                            _ => fg,
                        };
                        set_px(&mut buf, 320, col * 8 + hx * 2, row * 8 + py, c);
                        set_px(&mut buf, 320, col * 8 + hx * 2 + 1, row * 8 + py, c);
                    }
                } else {
                    let fgc = rgb((color_byte & 0x0F) as usize);
                    for px in 0..8usize {
                        let c = if byte & (0x80 >> px) != 0 { fgc } else { bg0 };
                        set_px(&mut buf, 320, col * 8 + px, row * 8 + py, c);
                    }
                }
            }
        }
    }
    buf
}

/// Extended background color mode  (320×200 RGB)
fn render_ecm(vic: &VicState, screen: &[u8], color: &[u8], chars: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 320 * 200 * 3];
    let bgs = [
        rgb(vic.background_color),
        rgb(vic.background_color1),
        rgb(vic.background_color2),
        rgb(vic.background_color3),
    ];
    for row in 0..25usize {
        for col in 0..40usize {
            let pos = row * 40 + col;
            let byte = screen[pos];
            let ch = (byte & 0x3F) as usize;
            let bg = bgs[((byte >> 6) & 0x03) as usize];
            let fg = rgb((color[pos] & 0x0F) as usize);
            for py in 0..8usize {
                let b = chars[(ch * 8 + py).min(chars.len() - 1)];
                for px in 0..8usize {
                    let c = if b & (0x80 >> px) != 0 { fg } else { bg };
                    set_px(&mut buf, 320, col * 8 + px, row * 8 + py, c);
                }
            }
        }
    }
    buf
}

/// Hi-res bitmap mode  (320×200 RGB)
fn render_hires_bitmap(_vic: &VicState, bitmap: &[u8], screen: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 320 * 200 * 3];
    for cr in 0..25usize {
        for cc in 0..40usize {
            let pos = cr * 40 + cc;
            let cb = screen[pos];
            let fg = rgb(((cb >> 4) & 0x0F) as usize);
            let bg = rgb((cb & 0x0F) as usize);
            let boff = cr * 320 + cc * 8;
            for py in 0..8usize {
                let b = bitmap[(boff + py).min(bitmap.len() - 1)];
                for px in 0..8usize {
                    let c = if b & (0x80 >> px) != 0 { fg } else { bg };
                    set_px(&mut buf, 320, cc * 8 + px, cr * 8 + py, c);
                }
            }
        }
    }
    buf
}

/// Multicolor bitmap mode  (160×200 → stretched to 320×200 RGB)
fn render_mc_bitmap(vic: &VicState, bitmap: &[u8], screen: &[u8], color: &[u8]) -> Vec<u8> {
    let mut buf160 = vec![0u8; 160 * 200 * 3];
    for cr in 0..25usize {
        for cc in 0..40usize {
            let pos = cr * 40 + cc;
            let cb = screen[pos];
            let c1 = rgb(((cb >> 4) & 0x0F) as usize);
            let c2 = rgb((cb & 0x0F) as usize);
            let c3 = rgb((color[pos] & 0x0F) as usize);
            let bg = rgb(vic.background_color);
            let boff = cr * 320 + cc * 8;
            for py in 0..8usize {
                let b = bitmap[(boff + py).min(bitmap.len() - 1)];
                for hx in 0..4usize {
                    let bits = (b >> (6 - hx * 2)) & 0x03;
                    let c = match bits {
                        0 => bg,
                        1 => c1,
                        2 => c2,
                        _ => c3,
                    };
                    set_px(&mut buf160, 160, cc * 4 + hx, cr * 8 + py, c);
                }
            }
        }
    }
    // Stretch 160 → 320 (nearest neighbor, double each column)
    let mut buf = vec![0u8; 320 * 200 * 3];
    for y in 0..200usize {
        for x in 0..160usize {
            let src = (y * 160 + x) * 3;
            let c = [buf160[src], buf160[src + 1], buf160[src + 2]];
            set_px(&mut buf, 320, x * 2, y, c);
            set_px(&mut buf, 320, x * 2 + 1, y, c);
        }
    }
    buf
}

/// Apply RSEL/CSEL display-window blanking and DEN (display enable) to a 320x200 RGB buffer.
///
/// When RSEL=0 the visible area shrinks to 24 rows: 8 pixels are blanked on the top
/// or bottom edge depending on YSCROLL.  When CSEL=0 the visible area shrinks to 38
/// columns: 8 pixels are blanked on the left or right edge depending on XSCROLL.
/// When DEN=0 the entire screen is blanked (replaced with the background color).
fn apply_blanking(buf: &mut [u8], vic: &VicState) {
    const W: usize = 320;
    const H: usize = 200;

    // DEN=0: entire display disabled — fill with background color
    if !vic.den {
        let bg = rgb(vic.background_color);
        for i in (0..W * H * 3).step_by(3) {
            buf[i] = bg[0];
            buf[i + 1] = bg[1];
            buf[i + 2] = bg[2];
        }
        return;
    }

    // RSEL/CSEL blanking strips use the border color (matches Python apply_rsel_csel_blanking)
    let bc = rgb(vic.border_color);

    // RSEL=0: 24-row mode — blank 8-pixel strip at top or bottom depending on YSCROLL
    if !vic.rsel {
        let y_start = if vic.yscroll >= 4 { H - 8 } else { 0 };
        for y in y_start..y_start + 8 {
            for x in 0..W {
                let i = (y * W + x) * 3;
                buf[i] = bc[0];
                buf[i + 1] = bc[1];
                buf[i + 2] = bc[2];
            }
        }
    }

    // CSEL=0: 38-column mode — blank 8-pixel strip at left or right depending on XSCROLL
    if !vic.csel {
        let x_start = if vic.xscroll >= 4 { W - 8 } else { 0 };
        for y in 0..H {
            for x in x_start..x_start + 8 {
                let i = (y * W + x) * 3;
                buf[i] = bc[0];
                buf[i + 1] = bc[1];
                buf[i + 2] = bc[2];
            }
        }
    }
}

/// Add a solid-color border (32 px each side) to a 320×200 image → 384×264
fn add_border(inner: &[u8], border_color: usize) -> Vec<u8> {
    const BW: usize = 32;
    const IW: usize = 320;
    const IH: usize = 200;
    const OW: usize = IW + BW * 2;
    const OH: usize = IH + BW * 2;
    let bc = rgb(border_color);
    let mut out = vec![0u8; OW * OH * 3];
    for y in 0..OH {
        for x in 0..OW {
            if x >= BW && x < BW + IW && y >= BW && y < BW + IH {
                let sx = x - BW;
                let sy = y - BW;
                let s = (sy * IW + sx) * 3;
                let d = (y * OW + x) * 3;
                out[d] = inner[s];
                out[d + 1] = inner[s + 1];
                out[d + 2] = inner[s + 2];
            } else {
                let d = (y * OW + x) * 3;
                out[d] = bc[0];
                out[d + 1] = bc[1];
                out[d + 2] = bc[2];
            }
        }
    }
    out
}

// ── sprites ───────────────────────────────────────────────────────────────────

struct Sprite {
    x: i32,
    y: i32,
    enabled: bool,
    x_expand: bool,
    y_expand: bool,
    multicolor: bool,
    #[allow(dead_code)]
    // stored but not used: Python tool does not implement behind-background priority
    priority: bool,
    color: usize,
    data_addr: u16,
}

fn parse_sprites(vic: &VicState) -> Vec<Sprite> {
    let r = &vic.raw;
    (0..8)
        .map(|i| {
            let x_low = r[i * 2] as i32;
            let x_msb = ((r[0x10] >> i) & 1) as i32;
            Sprite {
                x: x_low + x_msb * 256,
                y: r[i * 2 + 1] as i32,
                enabled: (r[0x15] >> i) & 1 == 1,
                x_expand: (r[0x1D] >> i) & 1 == 1,
                y_expand: (r[0x17] >> i) & 1 == 1,
                multicolor: (r[0x1C] >> i) & 1 == 1,
                priority: (r[0x1B] >> i) & 1 == 1, // $D01B
                color: (r[0x27 + i] & 0x0F) as usize,
                data_addr: 0, // filled in by caller with screen_mem pointers
            }
        })
        .collect()
}

/// Render one sprite to an RGBA patch.  Returns (rgba_data, screen_x, screen_y).
fn render_sprite(sprite: &Sprite, data: &[u8], vic: &VicState) -> (Vec<u8>, i32, i32) {
    let bw = 24usize * if sprite.x_expand { 2 } else { 1 };
    let bh = 21usize * if sprite.y_expand { 2 } else { 1 };
    let mut rgba = vec![0u8; bw * bh * 4];

    let sp_c = rgb(sprite.color);
    let mc0 = rgb(vic.sprite_multicolor0);
    let mc1 = rgb(vic.sprite_multicolor1);

    for row in 0..21usize {
        if row * 3 + 2 >= data.len() {
            break;
        }
        let bits = ((data[row * 3] as u32) << 16)
            | ((data[row * 3 + 1] as u32) << 8)
            | (data[row * 3 + 2] as u32);

        if sprite.multicolor {
            for hx in 0..12usize {
                let bp = (bits >> (22 - hx * 2)) & 0x03;
                if bp == 0 {
                    continue;
                }
                let c = if bp == 1 {
                    mc0
                } else if bp == 2 {
                    sp_c
                } else {
                    mc1
                };
                let xb = hx * 2 * if sprite.x_expand { 2 } else { 1 };
                let yb = row * if sprite.y_expand { 2 } else { 1 };
                for dx in 0..(2 * if sprite.x_expand { 2 } else { 1 }) {
                    for dy in 0..(if sprite.y_expand { 2 } else { 1 }) {
                        let px = xb + dx;
                        let py = yb + dy;
                        if px < bw && py < bh {
                            let i = (py * bw + px) * 4;
                            rgba[i] = c[0];
                            rgba[i + 1] = c[1];
                            rgba[i + 2] = c[2];
                            rgba[i + 3] = 255;
                        }
                    }
                }
            }
        } else {
            for px_idx in 0..24usize {
                if bits & (1 << (23 - px_idx)) == 0 {
                    continue;
                }
                let xb = px_idx * if sprite.x_expand { 2 } else { 1 };
                let yb = row * if sprite.y_expand { 2 } else { 1 };
                for dx in 0..(if sprite.x_expand { 2 } else { 1 }) {
                    for dy in 0..(if sprite.y_expand { 2 } else { 1 }) {
                        let px = xb + dx;
                        let py = yb + dy;
                        if px < bw && py < bh {
                            let i = (py * bw + px) * 4;
                            rgba[i] = sp_c[0];
                            rgba[i + 1] = sp_c[1];
                            rgba[i + 2] = sp_c[2];
                            rgba[i + 3] = 255;
                        }
                    }
                }
            }
        }
    }

    let screen_x = sprite.x - 24;
    let screen_y = sprite.y - 50;
    (rgba, screen_x, screen_y)
}

/// Alpha-composite sprite RGBA patches onto the RGB screen buffer (384x264 with border).
///
/// The C64 VIC-II sprite priority bit (register $D01B) controls whether each sprite
/// appears in front of or behind background graphics.  We implement this with two
/// passes: behind-background sprites are drawn first (so background pixels paint over
/// them), then front sprites are drawn last (so they appear on top).  Within each
/// pass sprites are drawn highest-number-first so sprite 0 wins ties (lower number =
/// higher priority on real hardware).
fn overlay_sprites_on_buf(
    buf: &mut Vec<u8>,
    w: usize,
    h: usize,
    border: usize,
    sprites: &[Sprite],
    sprite_data: &[Option<Vec<u8>>],
    vic: &VicState,
) {
    let blit = |buf: &mut Vec<u8>, i: usize| {
        let sprite = &sprites[i];
        let data = match &sprite_data[i] {
            Some(d) => d,
            None => return,
        };
        let (rgba, sx, sy) = render_sprite(sprite, data, vic);
        let sw = 24 * if sprite.x_expand { 2 } else { 1 };
        let sh = 21 * if sprite.y_expand { 2 } else { 1 };
        for py in 0..sh {
            for px in 0..sw {
                let bx = sx + px as i32 + border as i32;
                let by = sy + py as i32 + border as i32;
                if bx < 0 || by < 0 || bx as usize >= w || by as usize >= h {
                    continue;
                }
                let src = (py * sw + px) * 4;
                if rgba[src + 3] == 0 {
                    continue;
                }
                let dst = (by as usize * w + bx as usize) * 3;
                buf[dst] = rgba[src];
                buf[dst + 1] = rgba[src + 1];
                buf[dst + 2] = rgba[src + 2];
            }
        }
    };

    // Single pass, sprites 7->0 (sprite 0 = highest priority, drawn last = on top).
    // Matches Python overlay_sprites(front_only=False): all sprites drawn regardless of
    // priority bit. True behind-background priority would require a separate background
    // mask which the Python tool also does not implement.
    for i in (0..8).rev() {
        if sprites[i].enabled {
            blit(buf, i);
        }
    }
}

// ── public entry point ────────────────────────────────────────────────────────

/// Capture a screenshot from the Ultimate 64 **without** starting video streaming.
///
/// `host`     – IP address (e.g. "192.168.1.64")
/// `password` – optional REST API password
///
/// Returns the path to the saved PNG on success.
///
/// This is a **blocking** function; wrap it in `tokio::task::spawn_blocking`.
pub fn capture_screenshot_via_api(host: &str, password: Option<String>) -> Result<String, String> {
    let api = U64Api::new(host, password);

    log::info!("screenshot_api: freezing machine on {}", host);
    api.pause().ok(); // ignore error – may already be paused

    let result = capture_inner(&api);

    log::info!("screenshot_api: resuming machine");
    if let Err(e) = api.resume() {
        log::warn!("screenshot_api: resume failed: {}", e);
    }

    result
}

fn capture_inner(api: &U64Api) -> Result<String, String> {
    // ── read VIC-II registers ─────────────────────────────────────────────────
    let vic_regs = api.read_mem(0xD000, 0x30)?;
    let cia2_pa = api.read_mem(0xDD00, 1)?[0];
    let vic = VicState::from_regs(&vic_regs, cia2_pa);

    log::info!("screenshot_api: mode = {}", vic.mode_name());
    log::info!("screenshot_api: VIC bank = ${:04X}", vic.vic_bank);
    log::info!(
        "screenshot_api: screen=${:04X} char=${:04X} bitmap=${:04X}",
        vic.screen_mem_addr,
        vic.char_mem_addr,
        vic.bitmap_mem_addr
    );

    // ── color RAM ($D800) ─────────────────────────────────────────────────────
    let color_mem = api.read_mem(0xD800, 1000)?;

    // ── screen memory ─────────────────────────────────────────────────────────
    let screen_mem = smart_read(api, vic.screen_mem_addr, 1024)?;

    // ── bitmap / character data ───────────────────────────────────────────────
    let (bitmap_mem, char_rom): (Option<Vec<u8>>, Option<Vec<u8>>) = if vic.bmm {
        let bm = smart_read(api, vic.bitmap_mem_addr, 8000)?;
        (Some(bm), None)
    } else {
        // Check whether VIC points at the built-in character ROM.
        // ROM is visible in VIC bank 0 ($0000) and bank 2 ($8000) at offset $1000-$1FFF.
        let uses_char_rom = (vic.vic_bank == 0x0000 || vic.vic_bank == 0x8000)
            && (vic.char_mem_offset >= 0x1000 && vic.char_mem_offset < 0x2000);

        let cr = if uses_char_rom {
            log::info!("screenshot_api: using embedded character ROM");
            embedded_char_rom()
        } else {
            smart_read(api, vic.char_mem_addr, 2048)?
        };
        (None, Some(cr))
    };

    // ── render ────────────────────────────────────────────────────────────────
    let screen_rgb: Vec<u8> = match (vic.bmm, vic.mcm, vic.ecm) {
        (true, true, _) => render_mc_bitmap(
            &vic,
            bitmap_mem.as_deref().unwrap(),
            &screen_mem,
            &color_mem,
        ),
        (true, false, _) => render_hires_bitmap(&vic, bitmap_mem.as_deref().unwrap(), &screen_mem),
        (false, _, true) => render_ecm(&vic, &screen_mem, &color_mem, char_rom.as_deref().unwrap()),
        (false, true, _) => {
            render_mc_text(&vic, &screen_mem, &color_mem, char_rom.as_deref().unwrap())
        }
        _ => render_text(&vic, &screen_mem, &color_mem, char_rom.as_deref().unwrap()),
    };

    // ── display blanking (RSEL/CSEL/DEN) ─────────────────────────────────────
    let mut screen_rgb = screen_rgb;
    apply_blanking(&mut screen_rgb, &vic);

    // ── border ────────────────────────────────────────────────────────────────
    let border = add_border(&screen_rgb, vic.border_color);
    let out_w = 384usize;
    let out_h = 264usize;
    let border_size = 32usize;
    let mut final_rgb = border;

    // ── sprites ───────────────────────────────────────────────────────────────
    let sprite_pointers = &screen_mem[0x3F8..0x400];
    let mut sprites = parse_sprites(&vic);

    // Fill in sprite data addresses from screen_mem pointers
    for (i, sprite) in sprites.iter_mut().enumerate() {
        sprite.data_addr = vic.vic_bank + sprite_pointers[i] as u16 * 64;
    }

    let enabled_count = sprites.iter().filter(|s| s.enabled).count();
    log::info!("screenshot_api: {} sprite(s) enabled", enabled_count);

    let mut sprite_data: Vec<Option<Vec<u8>>> = Vec::with_capacity(8);
    for sprite in &sprites {
        if sprite.enabled {
            match api.read_mem(sprite.data_addr, 64) {
                Ok(d) => sprite_data.push(Some(d)),
                Err(_) => sprite_data.push(None),
            }
        } else {
            sprite_data.push(None);
        }
    }

    if enabled_count > 0 {
        overlay_sprites_on_buf(
            &mut final_rgb,
            out_w,
            out_h,
            border_size,
            &sprites,
            &sprite_data,
            &vic,
        );
    }

    // ── save ──────────────────────────────────────────────────────────────────
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let out_dir = dirs::picture_dir()
        .or_else(dirs::home_dir)
        .ok_or("Could not find Pictures/Home directory")?
        .join("Ultimate64");

    std::fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output dir: {}", e))?;

    let path = out_dir.join(format!("u64_screenshot_{}.png", timestamp));

    let img = image::RgbImage::from_raw(out_w as u32, out_h as u32, final_rgb)
        .ok_or("Failed to assemble image buffer")?;

    img.save(&path)
        .map_err(|e| format!("Failed to save PNG: {}", e))?;

    log::info!("screenshot_api: saved {}", path.display());
    Ok(path.to_string_lossy().into_owned())
}
