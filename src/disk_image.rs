//! Disk image library for D64/D71 Commodore disk formats
//!
//! Provides functionality to:
//! - Detect disk image type (D64 or D71)
//! - Read directory listings
//! - Extract disk name and ID
//! - Convert PETSCII to displayable characters

use std::fs;
use std::path::Path;
use ultimate64::petscii::Petscii;

/// Disk image type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageKind {
    D64,
    D71,
}

impl std::fmt::Display for ImageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageKind::D64 => write!(f, "D64"),
            ImageKind::D71 => write!(f, "D71"),
        }
    }
}

/// File type in directory entry
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileType {
    Del,
    Seq,
    Prg,
    Usr,
    Rel,
    Unknown(u8),
}

impl std::fmt::Display for FileType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileType::Del => write!(f, "DEL"),
            FileType::Seq => write!(f, "SEQ"),
            FileType::Prg => write!(f, "PRG"),
            FileType::Usr => write!(f, "USR"),
            FileType::Rel => write!(f, "REL"),
            FileType::Unknown(t) => write!(f, "?{:02X}", t),
        }
    }
}

impl FileType {
    fn from_byte(b: u8) -> Self {
        match b & 0x07 {
            0 => FileType::Del,
            1 => FileType::Seq,
            2 => FileType::Prg,
            3 => FileType::Usr,
            4 => FileType::Rel,
            x => FileType::Unknown(x),
        }
    }
}

/// A directory entry from a disk image
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub file_type: FileType,
    pub size_blocks: u16,
    pub locked: bool,
    pub closed: bool,
}

impl DirEntry {
    /// Format as a C64-style directory line
    pub fn format_line(&self) -> String {
        let lock_char = if self.locked { '<' } else { ' ' };
        let closed_char = if !self.closed { '*' } else { ' ' };
        format!(
            "{:>4}  \"{:<16}\" {}{}{} ",
            self.size_blocks, self.name, closed_char, self.file_type, lock_char
        )
    }
}

/// Information about a disk image
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub kind: ImageKind,
    pub name: String,
    pub disk_id: String,
    pub dos_type: String,
    pub entries: Vec<DirEntry>,
    pub blocks_free: u16,
}

impl DiskInfo {
    /// Format the header line like C64 directory listing
    pub fn format_header(&self) -> String {
        format!("0 \"{}\" {} {}", self.name, self.disk_id, self.dos_type)
    }

    /// Format the footer line with blocks free
    pub fn format_footer(&self) -> String {
        format!("{} BLOCKS FREE.", self.blocks_free)
    }

    /// Get all lines formatted like C64 directory listing
    pub fn format_listing(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(self.format_header());
        for entry in &self.entries {
            lines.push(entry.format_line());
        }
        lines.push(self.format_footer());
        lines
    }
}

/// Detect disk image type from file size
pub fn detect_kind(len: usize) -> Option<ImageKind> {
    // Most common sizes (with and without error info blocks):
    // D64: 174_848 (35 tracks), 175_531 (35 + error info), 196_608 (40 tracks)
    // D71: 349_696 (70 tracks), 351_062 (70 + error info)
    match len {
        174_848 | 175_531 | 196_608 | 197_376 => Some(ImageKind::D64),
        349_696 | 351_062 => Some(ImageKind::D71),
        _ => None,
    }
}

/// Sectors per track for 1541 layout (also used by 1571 per side)
fn spt_1541(track: u8) -> Option<u8> {
    match track {
        1..=17 => Some(21),
        18..=24 => Some(19),
        25..=30 => Some(18),
        31..=35 => Some(17),
        36..=40 => Some(17), // Extended tracks (some D64 variants)
        _ => None,
    }
}

/// Compute byte offset for a given (track, sector).
/// Tracks are 1-based. Sector is 0-based.
pub fn ts_offset(track: u8, sector: u8, kind: ImageKind) -> Option<usize> {
    if track == 0 {
        return None;
    }

    // In D71, tracks 1..=35 are side 0, 36..=70 are side 1 (1541 layout repeated).
    let (side_track, side) = match (kind, track) {
        (ImageKind::D64, 1..=40) => (track, 0usize),
        (ImageKind::D71, 1..=35) => (track, 0usize),
        (ImageKind::D71, 36..=70) => (track - 35, 1usize),
        _ => return None,
    };

    let spt = spt_1541(side_track)?;
    if sector >= spt {
        return None;
    }

    // Count sectors before this track on one side
    let mut sectors_before = 0usize;
    for t in 1..side_track {
        sectors_before += spt_1541(t).unwrap_or(0) as usize;
    }

    // Add side offset (D71 only)
    if kind == ImageKind::D71 && side == 1 {
        // Total sectors on side 0 (tracks 1..=35)
        let mut side0 = 0usize;
        for t in 1..=35 {
            side0 += spt_1541(t).unwrap_or(0) as usize;
        }
        sectors_before += side0;
    }

    let sector_index = sectors_before + sector as usize;
    Some(sector_index * 256)
}

/// Read a sector from the disk image
fn read_sector(data: &[u8], track: u8, sector: u8, kind: ImageKind) -> Option<&[u8]> {
    let offset = ts_offset(track, sector, kind)?;
    if offset + 256 <= data.len() {
        Some(&data[offset..offset + 256])
    } else {
        None
    }
}

/// Convert PETSCII bytes to a displayable string using ultimate64 crate
fn petscii_to_string(bytes: &[u8]) -> String {
    let mut result = String::new();

    for &b in bytes {
        // Skip $A0 padding (PETSCII shifted space used for padding)
        if b == 0xA0 {
            continue;
        }

        // Try to find ASCII char that maps to this PETSCII code
        let ch = petscii_byte_to_char(b);
        result.push(ch);
    }

    result.trim_end().to_string()
}

/// Convert a single PETSCII byte to a displayable character
/// Uses ultimate64::petscii::Petscii for reverse lookup where possible
fn petscii_byte_to_char(petscii_code: u8) -> char {
    // Try to find which ASCII character produces this PETSCII code
    // by checking common printable characters
    for c in ' '..='~' {
        let petscii_bytes = Petscii::from_str_lossy(&c.to_string());
        if !petscii_bytes.is_empty() {
            let code = petscii_bytes[0];
            if code == petscii_code {
                return c;
            }
        }
    }

    // Fallback conversion for codes not found via Petscii lookup
    match petscii_code {
        0x00..=0x1F => ' ',                  // Control characters
        0x20 => ' ',                         // Space
        0x21..=0x3F => petscii_code as char, // Numbers and symbols (same as ASCII)
        0x40 => '@',
        0x41..=0x5A => petscii_code as char, // Uppercase A-Z
        0x5B => '[',
        0x5C => '£',
        0x5D => ']',
        0x5E => '↑',
        0x5F => '←',
        0x60 => '─',
        0x61..=0x7A => petscii_code as char, // Lowercase in shifted mode
        0x7B..=0x7F => '▒',
        0x80..=0x9F => '▒',
        0xA0 => ' ',        // Shifted space (padding)
        0xA1..=0xBF => '▒', // Graphics
        0xC0 => '─',
        0xC1..=0xDA => ((petscii_code - 0xC1) + b'A') as char, // Uppercase again
        0xDB..=0xFF => '▒',                                    // More graphics
    }
}

/// Count free blocks from BAM
fn count_free_blocks(data: &[u8], kind: ImageKind) -> u16 {
    let bam = match read_sector(data, 18, 0, kind) {
        Some(s) => s,
        None => return 0,
    };

    let mut free = 0u16;

    // BAM entries start at offset 4
    // Each track has 4 bytes: first byte is free sector count
    // For D64: tracks 1-35 (skip track 18 - directory track)
    for track in 1..=35 {
        if track == 18 {
            continue; // Don't count directory track
        }
        let offset = 4 + ((track - 1) as usize) * 4;
        if offset < bam.len() {
            free += bam[offset] as u16;
        }
    }

    // For D71, there's additional BAM info
    if kind == ImageKind::D71 {
        // D71 has a second BAM at track 53, sector 0 for tracks 36-70
        if let Some(bam2) = read_sector(data, 53, 0, kind) {
            for track in 1..=35 {
                // Skip track 53 (relative track 18 on side 2)
                if track == 18 {
                    continue;
                }
                let offset = ((track - 1) as usize) * 3;
                if offset < bam2.len() {
                    // Count free sectors - first byte of each 3-byte entry
                    free += (bam2[offset].count_ones()) as u16;
                }
            }
        }
    }

    free
}

/// Read disk information from a file path
pub fn read_disk_info(path: &Path) -> Result<DiskInfo, String> {
    let data = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;

    read_disk_info_from_bytes(&data)
}

/// Read disk information from raw bytes
pub fn read_disk_info_from_bytes(data: &[u8]) -> Result<DiskInfo, String> {
    let kind = detect_kind(data.len())
        .ok_or_else(|| format!("Unknown disk image format (size: {} bytes)", data.len()))?;

    // Read BAM sector (track 18, sector 0)
    let bam =
        read_sector(data, 18, 0, kind).ok_or_else(|| "Failed to read BAM sector".to_string())?;

    // Extract disk name (bytes 144-159, 16 characters)
    let name_bytes = &bam[144..160];
    let name = petscii_to_string(name_bytes);

    // Extract disk ID (bytes 162-163)
    let id_bytes = &bam[162..164];
    let disk_id = petscii_to_string(id_bytes);

    // Extract DOS type (bytes 165-166)
    let dos_bytes = &bam[165..167];
    let dos_type = petscii_to_string(dos_bytes);

    // Count free blocks
    let blocks_free = count_free_blocks(data, kind);

    // Read directory entries
    let entries = read_directory(data, kind)?;

    Ok(DiskInfo {
        kind,
        name,
        disk_id,
        dos_type,
        entries,
        blocks_free,
    })
}

/// Read all directory entries from the disk
fn read_directory(data: &[u8], kind: ImageKind) -> Result<Vec<DirEntry>, String> {
    let mut entries = Vec::new();

    // Directory starts at track 18, sector 1
    let mut track = 18u8;
    let mut sector = 1u8;
    let mut iterations = 0;

    // Follow the directory chain
    while track != 0 && iterations < 20 {
        // Safety limit
        iterations += 1;

        let dir_sector = match read_sector(data, track, sector, kind) {
            Some(s) => s,
            None => break,
        };

        // Each sector has 8 directory entries of 32 bytes each
        for i in 0..8 {
            let offset = i * 32;
            let entry_bytes = &dir_sector[offset..offset + 32];

            // Check if entry is used (file type byte != 0)
            let file_type_byte = entry_bytes[2];
            if file_type_byte == 0 {
                continue; // Unused entry
            }

            // Parse the entry
            let file_type = FileType::from_byte(file_type_byte);
            let closed = (file_type_byte & 0x80) != 0;
            let locked = (file_type_byte & 0x40) != 0;

            // Filename is bytes 5-20 (16 characters)
            let name_bytes = &entry_bytes[5..21];
            let name = petscii_to_string(name_bytes);

            // File size in blocks (bytes 30-31, little-endian)
            let size_blocks = (entry_bytes[30] as u16) | ((entry_bytes[31] as u16) << 8);

            entries.push(DirEntry {
                name,
                file_type,
                size_blocks,
                locked,
                closed,
            });
        }

        // Follow chain to next directory sector
        track = dir_sector[0];
        sector = dir_sector[1];
    }

    Ok(entries)
}

/// Quick check if a file appears to be a supported disk image
#[allow(dead_code)]
pub fn is_disk_image(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());

    matches!(ext.as_deref(), Some("d64") | Some("d71"))
}

/// Get a brief summary of the disk (for tooltips, etc.)
#[allow(dead_code)]
/// TODO: use it for tooltip
pub fn get_disk_summary(path: &Path) -> Result<String, String> {
    let info = read_disk_info(path)?;

    let file_count = info.entries.len();
    let prg_count = info
        .entries
        .iter()
        .filter(|e| e.file_type == FileType::Prg)
        .count();

    Ok(format!(
        "{}: \"{}\" - {} files ({} PRG), {} blocks free",
        info.kind, info.name, file_count, prg_count, info.blocks_free
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_kind() {
        assert_eq!(detect_kind(174_848), Some(ImageKind::D64));
        assert_eq!(detect_kind(175_531), Some(ImageKind::D64));
        assert_eq!(detect_kind(349_696), Some(ImageKind::D71));
        assert_eq!(detect_kind(351_062), Some(ImageKind::D71));
        assert_eq!(detect_kind(12345), None);
    }

    #[test]
    fn test_ts_offset() {
        // Track 1, sector 0 should be at offset 0
        assert_eq!(ts_offset(1, 0, ImageKind::D64), Some(0));

        // Track 1, sector 1 should be at offset 256
        assert_eq!(ts_offset(1, 1, ImageKind::D64), Some(256));

        // Track 18, sector 0 (BAM) - need to count all sectors in tracks 1-17
        // Tracks 1-17 have 21 sectors each = 17 * 21 = 357 sectors
        let expected_track18 = 357 * 256;
        assert_eq!(ts_offset(18, 0, ImageKind::D64), Some(expected_track18));

        // Invalid track
        assert_eq!(ts_offset(0, 0, ImageKind::D64), None);
    }

    #[test]
    fn test_file_type() {
        assert_eq!(FileType::from_byte(0x00), FileType::Del);
        assert_eq!(FileType::from_byte(0x01), FileType::Seq);
        assert_eq!(FileType::from_byte(0x02), FileType::Prg);
        assert_eq!(FileType::from_byte(0x82), FileType::Prg); // With closed bit
        assert_eq!(FileType::from_byte(0xC2), FileType::Prg); // With closed and locked
    }
}
