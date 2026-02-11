//! SID file header parser (PSID / RSID v1–v4)
//!
//! Provides header parsing, subsong counting, payload extraction,
//! and MD5 computation for HVSC Songlength database lookup.

#![allow(dead_code)]

use std::path::Path;

const MD5_HASH_SIZE: usize = 16;

/// Parsed SID file header with full metadata.
#[derive(Debug, Clone)]
pub struct SidHeader {
    pub magic: String,
    pub version: u16,
    pub data_offset: u16,
    pub load_address: u16,
    pub init_address: u16,
    pub play_address: u16,
    pub songs: u16,
    pub start_song: u16,
    pub speed: u32,
    pub name: String,
    pub author: String,
    pub released: String,
    pub is_pal: bool,
    pub is_rsid: bool,
    /// C64 addresses of extra SIDs (0 = unused). Index 0 = SID2, 1 = SID3.
    pub extra_sid_addrs: [u16; 2],
}

impl SidHeader {
    /// Number of SID chips the tune uses (1–3).
    pub fn num_sids(&self) -> usize {
        1 + self.extra_sid_addrs.iter().filter(|&&a| a != 0).count()
    }

    /// Frame rate in Hz.
    #[allow(dead_code)]
    pub fn frame_rate(&self) -> f64 {
        if self.is_pal { 50.0 } else { 60.0 }
    }

    /// Frame duration in microseconds.
    pub fn frame_us(&self) -> u64 {
        if self.is_pal { 20_000 } else { 16_667 }
    }

    /// Display name: "Author - Title" or just "Title" if no author.
    pub fn display_name(&self) -> String {
        if self.name.is_empty() {
            String::new()
        } else if self.author.is_empty() {
            self.name.clone()
        } else {
            format!("{} - {}", self.author, self.name)
        }
    }

    /// Video standard as string.
    pub fn video_standard(&self) -> &'static str {
        if self.is_pal { "PAL" } else { "NTSC" }
    }

    /// SID model info string (e.g., "1xSID" or "2xSID @ $D420").
    pub fn sid_model_info(&self) -> String {
        let count = self.num_sids();
        if count == 1 {
            "1xSID".to_string()
        } else {
            let addrs: Vec<String> = self
                .extra_sid_addrs
                .iter()
                .filter(|&&a| a != 0)
                .map(|a| format!("${:04X}", a))
                .collect();
            format!("{}xSID @ {}", count, addrs.join(", "))
        }
    }
}

/// A fully loaded SID file: header + extracted payload + raw bytes.
#[derive(Debug, Clone)]
pub struct SidFile {
    pub header: SidHeader,
    pub load_address: u16,
    pub payload: Vec<u8>,
    /// Full raw file bytes (needed for MD5 computation).
    pub raw: Vec<u8>,
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn read_be_u16(d: &[u8], o: usize) -> u16 {
    ((d[o] as u16) << 8) | d[o + 1] as u16
}

fn read_be_u32(d: &[u8], o: usize) -> u32 {
    ((d[o] as u32) << 24) | ((d[o + 1] as u32) << 16) | ((d[o + 2] as u32) << 8) | d[o + 3] as u32
}

fn read_string(d: &[u8], o: usize, len: usize) -> String {
    let s = &d[o..o + len];
    let end = s.iter().position(|&b| b == 0).unwrap_or(len);
    s[..end]
        .iter()
        .filter_map(|&b| {
            if b >= 32 && b < 127 {
                Some(b as char)
            } else {
                None
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Decode a SID address byte (from header offset $7A or $7B).
fn decode_sid_addr_byte(b: u8) -> u16 {
    if b >= 0x42 && (b <= 0x7F || b >= 0xE0) && (b & 1) == 0 {
        0xD000 | ((b as u16) << 4)
    } else {
        0
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Parse a SID file header from raw bytes.
pub fn parse_header(data: &[u8]) -> Result<SidHeader, String> {
    if data.len() < 0x76 {
        return Err("File too small for a SID header".into());
    }

    let magic = String::from_utf8_lossy(&data[0..4]).to_string();
    if magic != "PSID" && magic != "RSID" {
        return Err(format!("Not a SID file (magic={magic:?})"));
    }

    let is_rsid = magic == "RSID";
    let version = read_be_u16(data, 0x04);
    let mut is_pal = true;
    let mut extra_sid_addrs = [0u16; 2];

    if version >= 2 && data.len() >= 0x7C {
        let flags = read_be_u16(data, 0x76);
        is_pal = ((flags >> 2) & 0x03) != 2;

        if version >= 3 && data.len() > 0x7A {
            extra_sid_addrs[0] = decode_sid_addr_byte(data[0x7A]);
        }
        if version >= 4 && data.len() > 0x7B {
            extra_sid_addrs[1] = decode_sid_addr_byte(data[0x7B]);
        }
    }

    Ok(SidHeader {
        magic,
        version,
        data_offset: read_be_u16(data, 0x06),
        load_address: read_be_u16(data, 0x08),
        init_address: read_be_u16(data, 0x0A),
        play_address: read_be_u16(data, 0x0C),
        songs: read_be_u16(data, 0x0E),
        start_song: read_be_u16(data, 0x10).max(1),
        speed: read_be_u32(data, 0x12),
        name: read_string(data, 0x16, 32),
        author: read_string(data, 0x36, 32),
        released: read_string(data, 0x56, 32),
        is_pal,
        is_rsid,
        extra_sid_addrs,
    })
}

/// Load a complete SID file: header + payload extraction.
pub fn load_sid(data: &[u8]) -> Result<SidFile, String> {
    let header = parse_header(data)?;
    let ds = header.data_offset as usize;

    if ds >= data.len() {
        return Err("data_offset past end of file".into());
    }

    let (load_address, payload_start) = if header.load_address == 0 {
        if ds + 2 > data.len() {
            return Err("File too small for embedded load address".into());
        }
        let lo = data[ds] as u16;
        let hi = data[ds + 1] as u16;
        ((hi << 8) | lo, ds + 2)
    } else {
        (header.load_address, ds)
    };

    let payload = data[payload_start..].to_vec();

    Ok(SidFile {
        header,
        load_address,
        payload,
        raw: data.to_vec(),
    })
}

/// Quick parse to get just subsong count from a file path (for browser display).
/// Only reads first 16 bytes. Returns 1 on any error.
pub fn quick_subsong_count(path: &Path) -> u8 {
    if let Ok(file) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buffer = [0u8; 16];
        let mut reader = std::io::BufReader::new(file);
        if reader.read_exact(&mut buffer).is_ok() {
            if &buffer[0..4] == b"PSID" || &buffer[0..4] == b"RSID" {
                let songs = ((buffer[14] as u16) << 8) | (buffer[15] as u16);
                return if songs > 0 && songs <= 256 {
                    songs as u8
                } else {
                    1
                };
            }
        }
    }
    1
}

/// Compute raw MD5 hash of file data (for Songlength database lookup).
pub fn compute_md5(data: &[u8]) -> [u8; MD5_HASH_SIZE] {
    md5::compute(data).0
}

/// Compute MD5 hash as hex string (for Songlength database lookup).
pub fn compute_md5_hex(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

/// Convert a hex string to MD5 hash bytes.
pub fn hex_to_md5(hex_str: &str) -> Option<[u8; MD5_HASH_SIZE]> {
    if hex_str.len() != 32 {
        return None;
    }

    let mut result = [0u8; MD5_HASH_SIZE];
    for (i, byte) in result.iter_mut().enumerate() {
        let hex_byte = &hex_str[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(hex_byte, 16).ok()?;
    }
    Some(result)
}

/// Parse a time string from the Songlength database.
/// Supports formats: "M:SS", "M:SS.mmm", "H:MM:SS", "H:MM:SS.mmm", or plain seconds.
pub fn parse_time_string(s: &str) -> Option<u32> {
    let s = s.split('.').next().unwrap_or(s);
    let parts: Vec<&str> = s.split(':').collect();

    match parts.len() {
        1 => parts[0].parse().ok(),
        2 => {
            let minutes: u32 = parts[0].parse().ok()?;
            let seconds: u32 = parts[1].parse().ok()?;
            Some(minutes * 60 + seconds)
        }
        3 => {
            let hours: u32 = parts[0].parse().ok()?;
            let minutes: u32 = parts[1].parse().ok()?;
            let seconds: u32 = parts[2].parse().ok()?;
            Some(hours * 3600 + minutes * 60 + seconds)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_time_string() {
        assert_eq!(parse_time_string("3:00"), Some(180));
        assert_eq!(parse_time_string("1:30"), Some(90));
        assert_eq!(parse_time_string("0:05"), Some(5));
        assert_eq!(parse_time_string("1:00:00"), Some(3600));
        assert_eq!(parse_time_string("2:30.500"), Some(150));
        assert_eq!(parse_time_string("45"), Some(45));
    }

    #[test]
    fn test_hex_to_md5() {
        let hex = "d41d8cd98f00b204e9800998ecf8427e";
        let result = hex_to_md5(hex);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 16);
    }

    #[test]
    fn test_hex_to_md5_invalid() {
        assert!(hex_to_md5("short").is_none());
        assert!(hex_to_md5("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_none());
    }

    #[test]
    fn test_decode_sid_addr() {
        assert_eq!(decode_sid_addr_byte(0x42), 0xD420);
        assert_eq!(decode_sid_addr_byte(0x00), 0);
        assert_eq!(decode_sid_addr_byte(0x41), 0); // odd = invalid
    }

    #[test]
    fn test_sid_header_display_name() {
        let header = SidHeader {
            magic: "PSID".into(),
            version: 2,
            data_offset: 0x7C,
            load_address: 0,
            init_address: 0x1000,
            play_address: 0x1003,
            songs: 3,
            start_song: 1,
            speed: 0,
            name: "Commando".into(),
            author: "Rob Hubbard".into(),
            released: "1985 Elite".into(),
            is_pal: true,
            is_rsid: false,
            extra_sid_addrs: [0, 0],
        };
        assert_eq!(header.display_name(), "Rob Hubbard - Commando");
        assert_eq!(header.num_sids(), 1);
        assert_eq!(header.video_standard(), "PAL");
    }
}
