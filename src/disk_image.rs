//! Disk image library for D64/D71/D81 Commodore disk formats
//!
//! Provides functionality to:
//! - Detect and create disk images (D64, D71, D81)
//! - Read directory listings and disk metadata
//! - Extract disk name and ID from BAM/header sectors
//! - Convert PETSCII to displayable characters

use std::fs;
use std::path::Path;

use crate::petscii;

/// Disk image type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageKind {
    D64,
    D71,
    D81,
}

impl std::fmt::Display for ImageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageKind::D64 => write!(f, "D64"),
            ImageKind::D71 => write!(f, "D71"),
            ImageKind::D81 => write!(f, "D81"),
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
    /// Raw PETSCII bytes of the filename (16 bytes, $A0-padded)
    /// Used for pixel-accurate rendering of special characters
    pub raw_name: Vec<u8>,
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
        819_200 => Some(ImageKind::D81),
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

    // D81: 80 tracks × 40 sectors each, completely uniform
    if kind == ImageKind::D81 {
        if track < 1 || track > 80 || sector >= 40 {
            return None;
        }
        return Some(((track as usize - 1) * 40 + sector as usize) * 256);
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

/// Count free blocks from BAM
fn count_free_blocks(data: &[u8], kind: ImageKind) -> u16 {
    match kind {
        ImageKind::D81 => count_free_blocks_d81(data),
        _ => count_free_blocks_d64_d71(data, kind),
    }
}

fn count_free_blocks_d64_d71(data: &[u8], kind: ImageKind) -> u16 {
    let bam = match read_sector(data, 18, 0, kind) {
        Some(s) => s,
        None => return 0,
    };

    let mut free = 0u16;

    // BAM entries start at offset 4; 4 bytes per track, first byte = free sector count
    for track in 1u8..=35 {
        if track == 18 {
            continue;
        } // directory track
        let offset = 4 + (track as usize - 1) * 4;
        if offset < bam.len() {
            free += bam[offset] as u16;
        }
    }

    // D71 has a second BAM at track 53, sector 0 for the second side (tracks 36-70)
    if kind == ImageKind::D71 {
        if let Some(bam2) = read_sector(data, 53, 0, kind) {
            for track in 1u8..=35 {
                if track == 18 {
                    continue;
                }
                let offset = (track as usize - 1) * 3;
                if offset < bam2.len() {
                    free += bam2[offset] as u16;
                }
            }
        }
    }

    free
}

/// D81 stores free-sector counts in two BAM blocks at track 40, sectors 1 and 2.
/// Each entry is 6 bytes: 1 count byte + 5 bitmap bytes (40 bits).
fn count_free_blocks_d81(data: &[u8]) -> u16 {
    let mut free = 0u16;
    for bam_sector in [1u8, 2u8] {
        let bam = match read_sector(data, 40, bam_sector, ImageKind::D81) {
            Some(s) => s,
            None => continue,
        };
        // Entries start at offset 16; 6 bytes each; 40 tracks per BAM block
        for i in 0..40usize {
            let off = 16 + i * 6;
            if off < bam.len() {
                free += bam[off] as u16;
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

    // D81 header is at track 40, sector 0; D64/D71 header is at track 18, sector 0
    let (header_track, name_off, id_off, dos_off) = match kind {
        ImageKind::D81 => (40u8, 4usize, 22usize, 25usize),
        _ => (18u8, 144usize, 162usize, 165usize),
    };

    let bam = read_sector(data, header_track, 0, kind)
        .ok_or_else(|| "Failed to read header/BAM sector".to_string())?;

    let name = petscii::to_string(&bam[name_off..name_off + 16]);
    let disk_id = petscii::to_string(&bam[id_off..id_off + 2]);
    let dos_type = petscii::to_string(&bam[dos_off..dos_off + 2]);

    let blocks_free = count_free_blocks(data, kind);
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

    // D81 directory starts at track 40, sector 3; D64/D71 at track 18, sector 1
    let (mut track, mut sector) = match kind {
        ImageKind::D81 => (40u8, 3u8),
        _ => (18u8, 1u8),
    };
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
            let name = petscii::to_string(name_bytes);
            let raw_name = name_bytes.to_vec();

            // File size in blocks (bytes 30-31, little-endian)
            let size_blocks = (entry_bytes[30] as u16) | ((entry_bytes[31] as u16) << 8);

            entries.push(DirEntry {
                name,
                raw_name,
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
    crate::file_types::is_disk_image_path(path)
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

// ─── Disk image creation ──────────────────────────────────────────────────────

/// Write a PETSCII disk name into a 16-byte slice, padding with 0xA0 (shifted space).
fn write_petscii_name(buf: &mut [u8], name: &str) {
    for (i, slot) in buf.iter_mut().enumerate() {
        *slot = name
            .bytes()
            .nth(i)
            .map(|b| if b.is_ascii_lowercase() { b - 32 } else { b })
            .unwrap_or(0xA0);
    }
}

/// Create a blank, formatted D64 image (174,848 bytes).
///
/// `name` is the disk name (max 16 chars, PETSCII-uppercased automatically).
/// `disk_id` should be 5 chars in the form `"AB 2A"` — first two chars become the
/// disk ID, the last two become the DOS type byte pair in the BAM header.
pub fn build_blank_d64(name: &str, disk_id: &str) -> Vec<u8> {
    const SIZE: usize = 174_848;
    let mut img = vec![0u8; SIZE];

    // BAM sector at track 18, sector 0
    let bam = ts_offset(18, 0, ImageKind::D64).unwrap_or(0x16500);

    // Link: track 18 sector 1 (start of directory); DOS version 'A'
    img[bam] = 18;
    img[bam + 1] = 1;
    img[bam + 2] = 0x41; // 'A'
    img[bam + 3] = 0x00;

    // BAM entries: 4 bytes per track (free count + 3-byte sector bitmap)
    // Track zones and their sector counts / bitmasks:
    //   1-17:  21 sectors → 0x1FFFFF
    //  18-24:  19 sectors → 0x07FFFF
    //  25-30:  18 sectors → 0x03FFFF
    //  31-35:  17 sectors → 0x01FFFF
    let zone: &[(u8, u8, u8, u32)] = &[
        (1, 17, 21, 0x1FFFFF),
        (18, 24, 19, 0x07FFFF),
        (25, 30, 18, 0x03FFFF),
        (31, 35, 17, 0x01FFFF),
    ];
    for track in 1u8..=35 {
        if track == 18 {
            continue;
        } // directory track — leave zeroed
        let (spt, mask) = zone
            .iter()
            .find(|&&(lo, hi, _, _)| track >= lo && track <= hi)
            .map(|&(_, _, s, m)| (s, m))
            .unwrap_or((17, 0x01FFFF));
        let off = bam + 4 + (track as usize - 1) * 4;
        img[off] = spt;
        img[off + 1] = (mask & 0xFF) as u8;
        img[off + 2] = ((mask >> 8) & 0xFF) as u8;
        img[off + 3] = ((mask >> 16) & 0xFF) as u8;
    }

    // Disk name at offsets 144..160, padded with 0xA0
    write_petscii_name(&mut img[bam + 144..bam + 160], name);
    img[bam + 160] = 0xA0;
    img[bam + 161] = 0xA0;

    // Disk ID (first 2 chars) at 162..164
    let id_chars: Vec<u8> = disk_id
        .split_whitespace()
        .next()
        .unwrap_or("01")
        .bytes()
        .take(2)
        .collect();
    for (i, slot) in img[bam + 162..bam + 164].iter_mut().enumerate() {
        *slot = *id_chars.get(i).unwrap_or(&0xA0);
    }

    // Separator + DOS type at 164..167
    img[bam + 164] = 0xA0;
    let dos: Vec<u8> = disk_id
        .split_whitespace()
        .nth(1)
        .unwrap_or("2A")
        .bytes()
        .take(2)
        .collect();
    for (i, slot) in img[bam + 165..bam + 167].iter_mut().enumerate() {
        *slot = *dos.get(i).unwrap_or(&b'A');
    }

    // Directory sector: track 18 sector 1 — end of chain marker
    let dir = ts_offset(18, 1, ImageKind::D64).unwrap_or(0x16600);
    img[dir] = 0;
    img[dir + 1] = 0xFF;

    img
}

/// Create a blank, formatted D71 image (349,696 bytes).
///
/// The D71 is two back-to-back 1541 sides. Side 0 uses the same layout as D64;
/// side 1 has a secondary BAM at track 53, sector 0.
pub fn build_blank_d71(name: &str, disk_id: &str) -> Vec<u8> {
    let d64 = build_blank_d64(name, disk_id);
    let mut img = vec![0u8; 349_696];

    // Side 0: copy D64 layout
    img[..174_848].copy_from_slice(&d64[..174_848]);

    // Side 1 BAM at track 53, sector 0
    // Track 53 on D71 = track 18 of side 1 (relative track 53-35=18)
    if let Some(bam2) = ts_offset(53, 0, ImageKind::D71) {
        if bam2 + 256 <= img.len() {
            img[bam2] = 0; // no chain
            img[bam2 + 1] = 0xFF;
            img[bam2 + 2] = 0x44; // 'D' — 1571 DOS version

            // Side-1 BAM entries: 3 bytes each (free count + 2-byte bitmap)
            // Tracks 36-70 → relative tracks 1-35 on side 1
            let zone: &[(u8, u8, u8, u32)] = &[
                (1, 17, 21, 0x1FFFFF),
                (18, 24, 19, 0x07FFFF),
                (25, 30, 18, 0x03FFFF),
                (31, 35, 17, 0x01FFFF),
            ];
            for rel in 1u8..=35 {
                if rel == 18 {
                    continue;
                } // directory track on side 1
                let (spt, mask) = zone
                    .iter()
                    .find(|&&(lo, hi, _, _)| rel >= lo && rel <= hi)
                    .map(|&(_, _, s, m)| (s, m))
                    .unwrap_or((17, 0x01FFFF));
                let off = bam2 + (rel as usize - 1) * 3;
                if off + 3 <= img.len() {
                    img[off] = spt;
                    img[off + 1] = (mask & 0xFF) as u8;
                    img[off + 2] = ((mask >> 8) & 0xFF) as u8;
                }
            }
        }
    }

    img
}

/// Create a blank, formatted D81 image (819,200 bytes).
///
/// The 1581 uses 80 uniform tracks of 40 sectors each. The header block lives
/// at track 40, sector 0; two BAM blocks follow at sectors 1 and 2; the
/// directory starts at sector 3.
pub fn build_blank_d81(name: &str, disk_id: &str) -> Vec<u8> {
    const SPT: usize = 40;
    const TRACKS: usize = 80;
    let mut img = vec![0u8; TRACKS * SPT * 256];

    let off = |tr: usize, sc: usize| ((tr - 1) * SPT + sc) * 256;

    // Header block at track 40, sector 0
    let hdr = off(40, 0);
    img[hdr] = 40; // next: track 40
    img[hdr + 1] = 3; // next: sector 3 (first directory sector)
    img[hdr + 2] = 0x44; // DOS version 'D'
    img[hdr + 3] = 0xBB;
    write_petscii_name(&mut img[hdr + 4..hdr + 20], name);
    img[hdr + 20] = 0xA0;
    img[hdr + 21] = 0xA0;
    let id_chars: Vec<u8> = disk_id
        .split_whitespace()
        .next()
        .unwrap_or("01")
        .bytes()
        .take(2)
        .collect();
    for (i, slot) in img[hdr + 22..hdr + 24].iter_mut().enumerate() {
        *slot = *id_chars.get(i).unwrap_or(&0xA0);
    }
    img[hdr + 24] = 0xA0;
    let dos: Vec<u8> = disk_id
        .split_whitespace()
        .nth(1)
        .unwrap_or("3D")
        .bytes()
        .take(2)
        .collect();
    for (i, slot) in img[hdr + 25..hdr + 27].iter_mut().enumerate() {
        *slot = *dos.get(i).unwrap_or(&b'D');
    }

    // BAM blocks at sectors 1 and 2, each covering 40 tracks
    for (bam_idx, start_track) in [(1usize, 1usize), (2, 41)] {
        let bam = off(40, bam_idx);
        img[bam] = if bam_idx == 1 { 40 } else { 0 };
        img[bam + 1] = if bam_idx == 1 { 2 } else { 0xFF };
        img[bam + 2] = 0x44;
        img[bam + 3] = 0xBB;
        // 6-byte entries start at offset 16: [free_count, b0, b1, b2, b3, b4]
        for i in 0..40usize {
            let track = start_track + i;
            let entry = bam + 16 + i * 6;
            if entry + 6 > img.len() {
                break;
            }
            if track == 40 {
                // Directory track — mark as occupied (count=0)
                img[entry] = 0;
            } else {
                img[entry] = 40; // 40 sectors free
                                 // All 40 bits set across 5 bytes
                img[entry + 1] = 0xFF;
                img[entry + 2] = 0xFF;
                img[entry + 3] = 0xFF;
                img[entry + 4] = 0xFF;
                img[entry + 5] = 0xFF;
            }
        }
    }

    // Directory start: track 40 sector 3 — end of chain
    let dir = off(40, 3);
    img[dir] = 0;
    img[dir + 1] = 0xFF;

    img
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
    fn test_d81_ts_offset() {
        // D81: track 1 sector 0 → offset 0
        assert_eq!(ts_offset(1, 0, ImageKind::D81), Some(0));
        // D81: track 2 sector 0 → offset 40*256 = 10240
        assert_eq!(ts_offset(2, 0, ImageKind::D81), Some(10240));
        // D81: sector ≥ 40 invalid
        assert_eq!(ts_offset(1, 40, ImageKind::D81), None);
        // D81: track 0 invalid
        assert_eq!(ts_offset(0, 0, ImageKind::D81), None);
    }

    #[test]
    fn test_detect_kind_d81() {
        assert_eq!(detect_kind(819_200), Some(ImageKind::D81));
    }

    #[test]
    fn test_build_blank_d64() {
        let img = build_blank_d64("TESTDISK", "AB 2A");
        assert_eq!(img.len(), 174_848);
        // BAM sector at track 18 sector 0
        let bam = ts_offset(18, 0, ImageKind::D64).unwrap();
        assert_eq!(img[bam + 2], 0x41); // DOS version 'A'
                                        // First data track (track 1) should have 21 free sectors
        assert_eq!(img[bam + 4], 21);
        // Directory track (18) should have zero free sectors
        assert_eq!(img[bam + 4 + 17 * 4], 0);
    }

    #[test]
    fn test_build_blank_d71() {
        let img = build_blank_d71("SIDE2DISK", "CD 2A");
        assert_eq!(img.len(), 349_696);
    }

    #[test]
    fn test_build_blank_d81() {
        let img = build_blank_d81("EIGHTYONE", "EF 3D");
        assert_eq!(img.len(), 819_200);
        // Header at track 40 sector 0
        let hdr = ts_offset(40, 0, ImageKind::D81).unwrap();
        assert_eq!(img[hdr + 2], 0x44); // DOS version 'D'
    }

    #[test]
    fn test_roundtrip_d64() {
        // Build a disk then read it back — name and kind should match
        let img = build_blank_d64("HELLO WORLD", "12 2A");
        let info = read_disk_info_from_bytes(&img).expect("should parse");
        assert_eq!(info.kind, ImageKind::D64);
        assert_eq!(info.name.trim(), "HELLO WORLD");
        assert_eq!(info.entries.len(), 0);
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
