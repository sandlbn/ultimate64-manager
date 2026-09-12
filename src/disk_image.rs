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

impl ImageKind {
    /// Track that holds the BAM and directory. A file's first block is never
    /// here, which is what makes it a reliable test for a decorative entry.
    pub fn dir_track(self) -> u8 {
        match self {
            ImageKind::D64 | ImageKind::D71 => 18,
            ImageKind::D81 => 40,
        }
    }

    /// Highest track number on this format.
    pub fn track_count(self) -> u8 {
        match self {
            ImageKind::D64 => 35,
            ImageKind::D71 => 70,
            ImageKind::D81 => 80,
        }
    }
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
    /// First track of the file's data chain (0 if the entry has no data).
    pub first_track: u8,
    /// First sector of the file's data chain.
    pub first_sector: u8,
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

            // First track/sector of the file's data chain (bytes 3-4).
            let first_track = entry_bytes[3];
            let first_sector = entry_bytes[4];

            entries.push(DirEntry {
                name,
                raw_name,
                file_type,
                size_blocks,
                locked,
                closed,
                first_track,
                first_sector,
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

// ─── File extraction ──────────────────────────────────────────────────────────

/// Follow a CBM file's sector chain and return its raw bytes (including the
/// 2-byte load-address header for a PRG). Returns `None` on a malformed chain.
///
/// Each data sector begins with a 2-byte link `(next_track, next_sector)`. When
/// `next_track == 0` the sector is the last one and `next_sector` is the index
/// of the final used byte, so the payload is `bytes[2..=next_sector]`.
fn follow_file_chain(data: &[u8], kind: ImageKind, start_t: u8, start_s: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut track, mut sector) = (start_t, start_s);
    // A 1541 disk holds ~683 sectors; cap well above that to break loops on
    // a corrupt (self-referential) chain rather than spin forever.
    for _ in 0..4000 {
        if track == 0 {
            break;
        }
        let sec = read_sector(data, track, sector, kind)?;
        let next_track = sec[0];
        let next_sector = sec[1];
        if next_track == 0 {
            // Last sector: next_sector points at the last valid byte.
            let end = (next_sector as usize + 1).min(256);
            if end > 2 {
                out.extend_from_slice(&sec[2..end]);
            }
            return Some(out);
        }
        out.extend_from_slice(&sec[2..256]);
        track = next_track;
        sector = next_sector;
    }
    None
}

/// If the image contains exactly one file and it is a PRG, extract and return
/// `(name, prg_bytes)` ready to hand to `run_prg`. Returns `None` for empty,
/// multi-file, or non-PRG disks so the caller falls back to mount + autoload.
///
/// A single-PRG disk is the safe case for a direct `run_prg`: multi-file disks
/// usually carry a loader that expects the disk mounted in the drive, so those
/// must go through the mount path instead.
pub fn extract_single_prg(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let kind = detect_kind(data.len())?;
    let entries = read_directory(data, kind).ok()?;
    let files: Vec<&DirEntry> = entries
        .iter()
        .filter(|e| e.file_type != FileType::Del && e.first_track != 0)
        .collect();
    let [only] = files.as_slice() else {
        return None;
    };
    if only.file_type != FileType::Prg {
        return None;
    }
    let bytes = follow_file_chain(data, kind, only.first_track, only.first_sector)?;
    // A valid PRG has at least the 2-byte load address plus one byte.
    if bytes.len() < 3 {
        return None;
    }
    Some((only.name.trim().to_string(), bytes))
}

/// Extract the disk's **first** directory file when it is a PRG, ready to hand to
/// `run_prg`. This mirrors `LOAD"*",8,1`: the C64 loads the first real directory
/// entry, so we DMA-boot that same entry (to its embedded load address) — but
/// only when it is a PRG. If the first entry is SEQ/USR/etc. (or the disk is
/// GCR/unparseable) this returns `None` and the caller falls back to the
/// keyboard `LOAD"*",8,1` path, which loads whatever the first file is.
///
/// Unlike [`extract_single_prg`] (which requires a single-file disk and is used
/// for the no-mount fast path), this is used *after mounting* so multi-load
/// games and disk-based loaders still find the emulated drive present.
pub fn extract_first_prg(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let kind = detect_kind(data.len())?;
    let entries = read_directory(data, kind).ok()?;
    // The first *loadable* entry is what LOAD"*" targets. "Loadable" has to be
    // checked rather than assumed: demo disks routinely fill the directory with
    // decorative entries whose names are graphics characters and whose start
    // pointers aim at the directory track itself. Taking the literal first entry
    // there DMA-loads the directory as if it were code, and a real drive asked
    // to LOAD it simply hangs. See [`is_loadable`].
    let first = entries.iter().find(|e| is_loadable(e, kind))?;
    if first.file_type != FileType::Prg {
        return None;
    }
    let bytes = follow_file_chain(data, kind, first.first_track, first.first_sector)?;
    if bytes.len() < 3 {
        return None;
    }
    Some((first.name.trim().to_string(), bytes))
}

/// Whether a directory entry points at something that could really be a file.
///
/// Rejects scratched entries and the decorative ones demo disks use for
/// directory art, identified by a start pointer that cannot hold file data: the
/// directory track itself, track 0, or a track beyond the disk.
fn is_loadable(e: &DirEntry, kind: ImageKind) -> bool {
    if e.file_type == FileType::Del || e.first_track == 0 {
        return false;
    }
    // The directory lives here; a file's first block never does.
    if e.first_track == kind.dir_track() {
        return false;
    }
    e.first_track <= kind.track_count()
}

/// Whether the disk has any entry that could actually be loaded.
///
/// * `Some(true)`  — there is a real file to boot.
/// * `Some(false)` — the directory was read and holds nothing loadable. Side-B
///   disks of multi-part demos look like this: every entry is decoration. Typing
///   `LOAD"*",8,1` at such a disk makes the drive hand back the BAM as a program
///   at `$0042`, which overwrites zero page and the stack and hangs the machine,
///   so the caller should decline rather than fall back to the keyboard.
/// * `None` — the image could not be parsed at all (GCR, damaged). The keyboard
///   path is still the right fallback there, since the directory is simply
///   unknown rather than known-empty.
pub fn has_loadable_entry(data: &[u8]) -> Option<bool> {
    let kind = detect_kind(data.len())?;
    let entries = read_directory(data, kind).ok()?;
    Some(entries.iter().any(|e| is_loadable(e, kind)))
}

/// Start of BASIC ROM. A program whose bytes run past here cannot be started by
/// BASIC's `RUN`, however valid its stub looks.
pub const BASIC_ROM_START: u32 = 0xA000;

/// Whether this PRG is too large for BASIC to `RUN`, and so must be entered by
/// jumping to its `SYS` address instead.
///
/// `prg` includes the two-byte load address. This is not hypothetical: a 43 KB
/// demo loading at `$0801` ends at `$B2BA`, and every attempt to `RUN` it
/// returned silently to `READY.`
pub fn needs_direct_jump(prg: &[u8]) -> bool {
    if prg.len() < 3 {
        return false;
    }
    let load = u16::from_le_bytes([prg[0], prg[1]]) as u32;
    load + (prg.len() as u32 - 2) > BASIC_ROM_START
}

/// Parse the `SYS <address>` of a BASIC stub, the convention machine-code
/// programs use to start themselves.
///
/// Needed because "load it and RUN" is not always enough: a stub whose payload
/// runs past the start of BASIC ROM cannot be started by BASIC's RUN at all, so
/// the entry point has to be jumped to directly. `prg` includes the two-byte
/// load address.
pub fn basic_stub_sys_address(prg: &[u8]) -> Option<u16> {
    // 2-byte load address, then: next-line link (2), line number (2), token.
    let body = prg.get(2..)?;
    // 0x9E is the SYS token.
    let sys_at = body.iter().take(16).position(|&b| b == 0x9E)?;
    let digits: String = body
        .get(sys_at + 1..)?
        .iter()
        .skip_while(|&&b| b == b' ')
        .take_while(|&&b| b.is_ascii_digit())
        .map(|&b| b as char)
        .collect();
    digits.parse::<u16>().ok()
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
    fn test_extract_single_prg_none_on_blank() {
        let img = build_blank_d64("EMPTY", "00 2A");
        assert!(extract_single_prg(&img).is_none());
    }

    #[test]
    fn test_extract_single_prg_reads_chain() {
        // Blank disk, then hand-place one PRG: a data sector at track 1 sector 0
        // and a directory entry pointing at it.
        let mut img = build_blank_d64("ONEFILE", "01 2A");

        // Data sector at track 1, sector 0: last-sector link (0, last_used_idx).
        let data_off = ts_offset(1, 0, ImageKind::D64).unwrap();
        let payload = [0x01u8, 0x08, 0x99]; // load address $0800 + one byte
        img[data_off] = 0; // next track = 0 → last sector
        img[data_off + 1] = 2 + payload.len() as u8 - 1; // index of last used byte
        img[data_off + 2..data_off + 2 + payload.len()].copy_from_slice(&payload);

        // Directory entry 0 at track 18 sector 1. Bytes [0..2] double as the
        // sector chain link (0 = end of directory).
        let dir_off = ts_offset(18, 1, ImageKind::D64).unwrap();
        img[dir_off] = 0; // dir chain: end
        img[dir_off + 1] = 0xFF;
        img[dir_off + 2] = 0x82; // closed PRG
        img[dir_off + 3] = 1; // first track
        img[dir_off + 4] = 0; // first sector
        img[dir_off + 5] = b'P'; // name "P" then padded
        for b in img[dir_off + 6..dir_off + 21].iter_mut() {
            *b = 0xA0;
        }

        let (name, bytes) = extract_single_prg(&img).expect("one PRG present");
        assert_eq!(name, "P");
        assert_eq!(bytes, payload);
    }

    /// A payload that runs past the start of BASIC ROM cannot be started by
    /// `RUN` — the case that left a 43 KB demo sitting at `READY.` forever.
    #[test]
    fn oversized_programs_are_flagged_for_a_direct_jump() {
        // $0801 + 43449 bytes ends at $B2BA, well past $A000.
        let mut big = vec![0x01, 0x08];
        big.extend(std::iter::repeat(0u8).take(43_449));
        assert!(needs_direct_jump(&big));

        // An ordinary program is fine for RUN.
        let mut small = vec![0x01, 0x08];
        small.extend(std::iter::repeat(0u8).take(4_000));
        assert!(!needs_direct_jump(&small));

        assert!(!needs_direct_jump(&[0x01]), "runt input must not panic");
    }

    #[test]
    fn basic_stub_sys_address_is_parsed() {
        // 10 SYS 2064
        let prg = [
            0x01, 0x08, 0x0b, 0x08, 0x3e, 0x0c, 0x9e, b'2', b'0', b'6', b'4', 0x00, 0x00, 0x00,
        ];
        assert_eq!(basic_stub_sys_address(&prg), Some(2064));
        // No SYS token -> nothing to jump to.
        assert_eq!(basic_stub_sys_address(&[0x01, 0x08, 0, 0, 0, 0, 0]), None);
    }

    /// Decorative directory entries point at the directory track, so a LOAD
    /// takes the BAM as a program (load address `$0042`) and hangs the machine.
    /// They must never be chosen.
    #[test]
    fn directory_art_entries_are_not_loadable() {
        let art = DirEntry {
            name: "ART".into(),
            raw_name: vec![0xA0; 16],
            file_type: FileType::Prg,
            size_blocks: 0,
            locked: true,
            closed: true,
            first_track: 18, // the directory track itself
            first_sector: 0,
        };
        assert!(!is_loadable(&art, ImageKind::D64));

        let real = DirEntry {
            first_track: 19,
            ..art.clone()
        };
        assert!(is_loadable(&real, ImageKind::D64));

        let off_disk = DirEntry {
            first_track: 200,
            ..art.clone()
        };
        assert!(!is_loadable(&off_disk, ImageKind::D64));

        let scratched = DirEntry {
            first_track: 19,
            file_type: FileType::Del,
            ..art.clone()
        };
        assert!(!is_loadable(&scratched, ImageKind::D64));
    }

    #[test]
    fn test_extract_first_prg_picks_first_when_prg() {
        // First dir entry is a PRG (points at a data sector), second is a SEQ.
        // extract_first_prg must return the first (PRG) file.
        let mut img = build_blank_d64("TWOFILES", "01 2A");
        let data_off = ts_offset(1, 0, ImageKind::D64).unwrap();
        let payload = [0x00u8, 0x08, 0x99];
        img[data_off] = 0;
        img[data_off + 1] = 2 + payload.len() as u8 - 1;
        img[data_off + 2..data_off + 2 + payload.len()].copy_from_slice(&payload);

        let dir_off = ts_offset(18, 1, ImageKind::D64).unwrap();
        img[dir_off] = 0; // dir chain end
        img[dir_off + 1] = 0xFF;
        // entry 0: closed PRG "A" at track 1 sector 0
        img[dir_off + 2] = 0x82;
        img[dir_off + 3] = 1;
        img[dir_off + 4] = 0;
        img[dir_off + 5] = b'A';
        for b in img[dir_off + 6..dir_off + 21].iter_mut() {
            *b = 0xA0;
        }
        // entry 1 (offset +32): closed SEQ "B"
        img[dir_off + 32 + 2] = 0x81; // closed SEQ
        img[dir_off + 32 + 3] = 1;
        img[dir_off + 32 + 4] = 1;
        img[dir_off + 32 + 5] = b'B';
        for b in img[dir_off + 32 + 6..dir_off + 32 + 21].iter_mut() {
            *b = 0xA0;
        }

        let (name, bytes) = extract_first_prg(&img).expect("first PRG present");
        assert_eq!(name, "A");
        assert_eq!(bytes, payload);
    }

    #[test]
    fn test_extract_first_prg_none_when_first_is_seq() {
        // First dir entry is a SEQ → LOAD"*" would load it; we return None so the
        // keyboard path handles it rather than DMA-booting a later PRG.
        let mut img = build_blank_d64("SEQFIRST", "01 2A");
        let dir_off = ts_offset(18, 1, ImageKind::D64).unwrap();
        img[dir_off] = 0;
        img[dir_off + 1] = 0xFF;
        img[dir_off + 2] = 0x81; // closed SEQ
        img[dir_off + 3] = 1;
        img[dir_off + 4] = 0;
        img[dir_off + 5] = b'S';
        for b in img[dir_off + 6..dir_off + 21].iter_mut() {
            *b = 0xA0;
        }
        assert!(extract_first_prg(&img).is_none());
    }

    #[test]
    fn test_extract_first_prg_none_on_gcr() {
        // Unparseable length (not a decoded D64/D71/D81) → None → keyboard path.
        assert!(extract_first_prg(&vec![0u8; 12345]).is_none());
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

#[cfg(test)]
mod real_image_inspection {
    use super::*;

    /// Print what the boot logic decides for a real image. Diagnostic, not an
    /// assertion: point it at a disk with `U64_TEST_D64=<path>`.
    #[test]
    #[ignore = "diagnostic: set U64_TEST_D64=<path to a .d64>"]
    fn inspect_real_d64() {
        let Ok(path) = std::env::var("U64_TEST_D64") else {
            eprintln!("SKIP: set U64_TEST_D64");
            return;
        };
        let data = std::fs::read(&path).expect("read image");
        let kind = detect_kind(data.len());
        println!("{path}\n  kind={kind:?} len={}", data.len());
        match read_directory(&data, kind.expect("kind")) {
            Ok(entries) => {
                for (i, e) in entries.iter().take(10).enumerate() {
                    println!(
                        "  [{i}] {:?} t/s={}/{} blocks={} loadable={} {:?}",
                        e.file_type,
                        e.first_track,
                        e.first_sector,
                        e.size_blocks,
                        is_loadable(e, kind.unwrap()),
                        e.name
                    );
                }
            }
            Err(e) => println!("  directory unreadable: {e}"),
        }
        match extract_first_prg(&data) {
            Some((name, prg)) => {
                let load = u16::from_le_bytes([prg[0], prg[1]]);
                println!(
                    "  -> chosen {:?}: {} bytes, load ${:04X}, end ${:04X}",
                    name,
                    prg.len() - 2,
                    load,
                    load as u32 + prg.len() as u32 - 2
                );
                println!(
                    "  -> needs_direct_jump={} sys={:?}",
                    needs_direct_jump(&prg),
                    basic_stub_sys_address(&prg)
                );
            }
            None => println!("  -> extract_first_prg: None (keyboard LOAD fallback)"),
        }
    }
}
