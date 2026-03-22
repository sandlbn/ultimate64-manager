//! Directory content preview module
//! Handles loading and displaying text files (readme, .txt, .atxt, .nfo, .diz)
//! and image files (.png, .jpg, .jpeg, .gif, .bmp) from the file browser.

use std::path::{Path, PathBuf};

use crate::disk_image::DiskInfo;
use crate::petscii;

/// Supported preview content types
#[derive(Debug, Clone)]
pub enum ContentPreview {
    /// Text file content with filename and content
    Text {
        filename: String,
        content: String,
        line_count: usize,
    },
    /// Image file with raw bytes and dimensions
    Image {
        filename: String,
        data: Vec<u8>,
        width: u32,
        height: u32,
    },
}

// ─────────────────────────────────────────────────────────────────
//  C64-style 8×8 bitmap font
//  Each character is 8 bytes; each byte is one row (MSB = leftmost pixel).
//  Characters are ordered by ASCII code starting at 0x20 (space).
//  This is an original-style recreation for the printable ASCII range
//  used in C64 directory listings (space, 0-9, A-Z, punctuation).
// ─────────────────────────────────────────────────────────────────

/// Return the 8-byte bitmap for an ASCII character (0x20..=0x5F supported).
/// Falls back to all-zero (space) for unsupported characters.
fn char_bitmap(ch: u8) -> [u8; 8] {
    match ch {
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'!' => [0x18, 0x18, 0x18, 0x18, 0x00, 0x00, 0x18, 0x00],
        b'"' => [0x66, 0x66, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'#' => [0x6C, 0x6C, 0xFE, 0x6C, 0xFE, 0x6C, 0x6C, 0x00],
        b'$' => [0x30, 0x7C, 0xC0, 0x78, 0x0C, 0xF8, 0x30, 0x00],
        b'%' => [0x00, 0xC6, 0xCC, 0x18, 0x30, 0x66, 0xC6, 0x00],
        b'&' => [0x38, 0x6C, 0x38, 0x76, 0xDC, 0xCC, 0x76, 0x00],
        b'\'' => [0x60, 0x60, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'(' => [0x18, 0x30, 0x60, 0x60, 0x60, 0x30, 0x18, 0x00],
        b')' => [0x60, 0x30, 0x18, 0x18, 0x18, 0x30, 0x60, 0x00],
        b'*' => [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
        b'+' => [0x00, 0x30, 0x30, 0xFC, 0x30, 0x30, 0x00, 0x00],
        b',' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x30, 0x60],
        b'-' => [0x00, 0x00, 0x00, 0xFC, 0x00, 0x00, 0x00, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x30, 0x00],
        b'/' => [0x06, 0x0C, 0x18, 0x30, 0x60, 0xC0, 0x80, 0x00],
        b'0' => [0x7C, 0xC6, 0xCE, 0xDE, 0xF6, 0xE6, 0x7C, 0x00],
        b'1' => [0x30, 0x70, 0x30, 0x30, 0x30, 0x30, 0xFC, 0x00],
        b'2' => [0x78, 0xCC, 0x0C, 0x38, 0x60, 0xCC, 0xFC, 0x00],
        b'3' => [0x78, 0xCC, 0x0C, 0x38, 0x0C, 0xCC, 0x78, 0x00],
        b'4' => [0x1C, 0x3C, 0x6C, 0xCC, 0xFE, 0x0C, 0x1E, 0x00],
        b'5' => [0xFC, 0xC0, 0xF8, 0x0C, 0x0C, 0xCC, 0x78, 0x00],
        b'6' => [0x38, 0x60, 0xC0, 0xF8, 0xCC, 0xCC, 0x78, 0x00],
        b'7' => [0xFC, 0xCC, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x00],
        b'8' => [0x78, 0xCC, 0xCC, 0x78, 0xCC, 0xCC, 0x78, 0x00],
        b'9' => [0x78, 0xCC, 0xCC, 0x7C, 0x0C, 0x18, 0x70, 0x00],
        b':' => [0x00, 0x30, 0x30, 0x00, 0x00, 0x30, 0x30, 0x00],
        b';' => [0x00, 0x30, 0x30, 0x00, 0x00, 0x30, 0x30, 0x60],
        b'<' => [0x18, 0x30, 0x60, 0xC0, 0x60, 0x30, 0x18, 0x00],
        b'=' => [0x00, 0x00, 0xFC, 0x00, 0x00, 0xFC, 0x00, 0x00],
        b'>' => [0x60, 0x30, 0x18, 0x0C, 0x18, 0x30, 0x60, 0x00],
        b'?' => [0x78, 0xCC, 0x0C, 0x18, 0x30, 0x00, 0x30, 0x00],
        b'@' => [0x7C, 0xC6, 0xDE, 0xDE, 0xDE, 0xC0, 0x78, 0x00],
        b'A' => [0x30, 0x78, 0xCC, 0xCC, 0xFC, 0xCC, 0xCC, 0x00],
        b'B' => [0xFC, 0x66, 0x66, 0x7C, 0x66, 0x66, 0xFC, 0x00],
        b'C' => [0x3C, 0x66, 0xC0, 0xC0, 0xC0, 0x66, 0x3C, 0x00],
        b'D' => [0xF8, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0xF8, 0x00],
        b'E' => [0xFE, 0x62, 0x68, 0x78, 0x68, 0x62, 0xFE, 0x00],
        b'F' => [0xFE, 0x62, 0x68, 0x78, 0x68, 0x60, 0xF0, 0x00],
        b'G' => [0x3C, 0x66, 0xC0, 0xC0, 0xCE, 0x66, 0x3E, 0x00],
        b'H' => [0xCC, 0xCC, 0xCC, 0xFC, 0xCC, 0xCC, 0xCC, 0x00],
        b'I' => [0x78, 0x30, 0x30, 0x30, 0x30, 0x30, 0x78, 0x00],
        b'J' => [0x1E, 0x0C, 0x0C, 0x0C, 0xCC, 0xCC, 0x78, 0x00],
        b'K' => [0xE6, 0x66, 0x6C, 0x78, 0x6C, 0x66, 0xE6, 0x00],
        b'L' => [0xF0, 0x60, 0x60, 0x60, 0x62, 0x66, 0xFE, 0x00],
        b'M' => [0xC6, 0xEE, 0xFE, 0xFE, 0xD6, 0xC6, 0xC6, 0x00],
        b'N' => [0xC6, 0xE6, 0xF6, 0xDE, 0xCE, 0xC6, 0xC6, 0x00],
        b'O' => [0x38, 0x6C, 0xC6, 0xC6, 0xC6, 0x6C, 0x38, 0x00],
        b'P' => [0xFC, 0x66, 0x66, 0x7C, 0x60, 0x60, 0xF0, 0x00],
        b'Q' => [0x78, 0xCC, 0xCC, 0xCC, 0xDC, 0x78, 0x1C, 0x00],
        b'R' => [0xFC, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0xE6, 0x00],
        b'S' => [0x78, 0xCC, 0xE0, 0x70, 0x1C, 0xCC, 0x78, 0x00],
        b'T' => [0xFC, 0xB4, 0x30, 0x30, 0x30, 0x30, 0x78, 0x00],
        b'U' => [0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0xFC, 0x00],
        b'V' => [0xCC, 0xCC, 0xCC, 0xCC, 0xCC, 0x78, 0x30, 0x00],
        b'W' => [0xC6, 0xC6, 0xC6, 0xD6, 0xFE, 0xEE, 0xC6, 0x00],
        b'X' => [0xC6, 0xC6, 0x6C, 0x38, 0x38, 0x6C, 0xC6, 0x00],
        b'Y' => [0xCC, 0xCC, 0xCC, 0x78, 0x30, 0x30, 0x78, 0x00],
        b'Z' => [0xFE, 0xC6, 0x8C, 0x18, 0x32, 0x66, 0xFE, 0x00],
        b'[' => [0x78, 0x60, 0x60, 0x60, 0x60, 0x60, 0x78, 0x00],
        b'\\' => [0xC0, 0x60, 0x30, 0x18, 0x0C, 0x06, 0x02, 0x00],
        b']' => [0x78, 0x18, 0x18, 0x18, 0x18, 0x18, 0x78, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

/// Map a PETSCII byte to an 8×8 bitmap for rendering.
/// Handles the full PETSCII character set including graphics characters.
fn petscii_bitmap(petscii: u8) -> [u8; 8] {
    match petscii {
        // $A0 = shifted space (padding) → blank
        0xA0 => char_bitmap(b' '),
        // Standard printable ASCII / PETSCII uppercase ($20-$5F)
        0x20..=0x5F => char_bitmap(petscii.to_ascii_uppercase()),
        // Lowercase PETSCII ($61-$7A) → uppercase glyphs
        0x61..=0x7A => char_bitmap(petscii - 0x20),
        // $C1-$DA = alternate PETSCII uppercase range — these are GRAPHIC characters
        // on screen, NOT letters. The C64 displays them as reverse-video blocks.
        // Render as a checkerboard / partial block pattern
        0xC1..=0xDA => {
            // Use a distinctive pattern so graphics chars are visible
            let v = (petscii - 0xC1) as usize;
            // Alternate between a few block patterns based on the char value
            match v % 4 {
                0 => [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55], // checkerboard
                1 => [0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00], // horizontal stripes
                2 => [0xCC, 0xCC, 0xCC, 0xCC, 0x33, 0x33, 0x33, 0x33], // vertical quarters
                _ => [0xF0, 0xF0, 0xF0, 0xF0, 0x0F, 0x0F, 0x0F, 0x0F], // half blocks
            }
        }
        // Other graphics ranges — filled or partial blocks
        0xA1..=0xBF => [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], // full block
        0x7B..=0x7E => [0xF0, 0xF0, 0xF0, 0xF0, 0x00, 0x00, 0x00, 0x00], // upper half
        0xDB..=0xFF => [0x00, 0x00, 0x00, 0x00, 0xF0, 0xF0, 0xF0, 0xF0], // lower half
        0x7F => [0x18, 0x3C, 0x7E, 0xFF, 0x7E, 0x3C, 0x18, 0x00],        // diamond
        0x40 | 0x60 => [0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00], // horiz line
        // Dot for anything else
        _ => [0x00, 0x00, 0x18, 0x18, 0x18, 0x00, 0x00, 0x00],
    }
}

/// Render a C64-style directory listing into a PNG image.
///
/// Returns raw PNG bytes that can be used as an iced image handle.
/// The image uses authentic C64 colours: blue background, light blue border,
/// white text — matching the real C64 directory listing appearance.
pub fn render_disk_listing_image(disk_info: &DiskInfo) -> Vec<u8> {
    // C64 screen: 40 columns, each char 8×8 px
    // Use 4-char border on left/right and 2-char border top/bottom for a spacious look
    const COLS: usize = 40;
    const CHAR_W: usize = 8;
    const CHAR_H: usize = 8;
    const BORDER_X: usize = 4; // border width in chars
    const BORDER_Y: usize = 2; // border height in chars
    const SCALE: usize = 2; // pixel scale factor — 2× makes chars crisply readable

    // C64 authentic colours (RGB)
    const BG: [u8; 3] = [0x35, 0x28, 0x79]; // C64 blue background
    const BORDER_COL: [u8; 3] = [0x70, 0x5B, 0xD5]; // C64 light blue border
    const CYAN: [u8; 3] = [0x5F, 0xD5, 0xCB]; // header / footer
    const LIGHT_GREEN: [u8; 3] = [0x76, 0xD5, 0x5F]; // PRG files
    const LIGHT_BLUE: [u8; 3] = [0x70, 0xA4, 0xD5]; // SEQ files
    const LIGHT_RED: [u8; 3] = [0xD5, 0x6F, 0x5F]; // other file types

    // Build lines: (raw bytes, colour)
    // All bytes are PETSCII — uppercase letters are same as ASCII
    let mut lines: Vec<(Vec<u8>, [u8; 3])> = Vec::new();

    // Header line: 0 "DISKNAME         " ID DOS
    {
        let mut h: Vec<u8> = Vec::new();
        h.push(b'0');
        h.push(b' ');
        h.push(b'"');
        // Disk name: raw PETSCII bytes from disk_info, padded to 16
        let mut name_len = 0;
        for &b in disk_info.name.as_bytes() {
            h.push(b);
            name_len += 1;
            if name_len >= 16 {
                break;
            }
        }
        for _ in name_len..16 {
            h.push(b' ');
        }
        h.push(b'"');
        h.push(b' ');
        for b in disk_info.disk_id.bytes() {
            h.push(b);
        }
        h.push(b' ');
        for b in disk_info.dos_type.bytes() {
            h.push(b);
        }
        lines.push((h, CYAN));
    }

    // One line per directory entry
    for entry in &disk_info.entries {
        let colour = match entry.file_type {
            crate::disk_image::FileType::Prg => LIGHT_GREEN,
            crate::disk_image::FileType::Seq => LIGHT_BLUE,
            _ => LIGHT_RED,
        };
        let lock_char = if entry.locked { b'<' } else { b' ' };
        let closed_char = if !entry.closed { b'*' } else { b' ' };
        let file_type_str = format!("{}", entry.file_type);

        // Build the directory line matching real C64 format:
        // "NNN   \"FILENAME        \" TYP"
        let mut raw_line: Vec<u8> = Vec::new();
        for b in format!("{:<4}", entry.size_blocks).bytes() {
            raw_line.push(b);
        }
        raw_line.push(b' ');
        raw_line.push(b'"');
        // Name: walk raw PETSCII bytes, stop at $A0 padding
        let name_len = entry
            .raw_name
            .iter()
            .take(16)
            .position(|&b| b == 0xA0)
            .unwrap_or(entry.raw_name.len().min(16));
        for &b in &entry.raw_name[..name_len] {
            raw_line.push(b);
        }
        for _ in name_len..16 {
            raw_line.push(b' ');
        }
        raw_line.push(b'"');
        raw_line.push(b' ');
        raw_line.push(closed_char);
        for b in file_type_str.bytes() {
            raw_line.push(b);
        }
        raw_line.push(lock_char);

        lines.push((raw_line, colour));
    }

    // Footer
    {
        let footer_str = format!("{} BLOCKS FREE.", disk_info.blocks_free);
        lines.push((footer_str.into_bytes(), CYAN));
    }

    // Image dimensions (before scaling)
    let total_rows = lines.len() + BORDER_Y * 2;
    let img_w_chars = COLS + BORDER_X * 2;
    let img_w = img_w_chars * CHAR_W * SCALE;
    let img_h = total_rows * CHAR_H * SCALE;

    // Allocate RGBA pixel buffer, fill with border colour
    let mut pixels = vec![0u8; img_w * img_h * 4];
    for i in 0..img_w * img_h {
        pixels[i * 4] = BORDER_COL[0];
        pixels[i * 4 + 1] = BORDER_COL[1];
        pixels[i * 4 + 2] = BORDER_COL[2];
        pixels[i * 4 + 3] = 0xFF;
    }

    // Fill inner screen area with background colour
    let sx0 = BORDER_X * CHAR_W * SCALE;
    let sx1 = sx0 + COLS * CHAR_W * SCALE;
    let sy0 = BORDER_Y * CHAR_H * SCALE;
    let sy1 = img_h - BORDER_Y * CHAR_H * SCALE;
    for y in sy0..sy1 {
        for x in sx0..sx1 {
            let i = (y * img_w + x) * 4;
            pixels[i] = BG[0];
            pixels[i + 1] = BG[1];
            pixels[i + 2] = BG[2];
            pixels[i + 3] = 0xFF;
        }
    }

    // Helper: set a scaled pixel
    let mut set_pixel = |x: usize, y: usize, col: [u8; 3]| {
        for dy in 0..SCALE {
            for dx in 0..SCALE {
                let px = x * SCALE + dx;
                let py = y * SCALE + dy;
                if px < img_w && py < img_h {
                    let i = (py * img_w + px) * 4;
                    pixels[i] = col[0];
                    pixels[i + 1] = col[1];
                    pixels[i + 2] = col[2];
                    pixels[i + 3] = 0xFF;
                }
            }
        }
    };

    // Draw each text line using PETSCII byte values directly
    for (line_idx, (bytes, colour)) in lines.iter().enumerate() {
        let char_y0 = BORDER_Y * CHAR_H + line_idx * CHAR_H;
        for (col_idx, &b) in bytes.iter().enumerate() {
            if col_idx >= COLS {
                break;
            }
            let char_x0 = BORDER_X * CHAR_W + col_idx * CHAR_W;
            let bitmap = petscii_bitmap(b);
            for row in 0..CHAR_H {
                let byte = bitmap[row];
                for bit in 0..CHAR_W {
                    // MSB = leftmost pixel
                    if (byte >> (7 - bit)) & 1 == 1 {
                        set_pixel(char_x0 + bit, char_y0 + row, *colour);
                    }
                }
            }
        }
    }

    // Encode as PNG — write raw RGBA rows directly
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(
            std::io::Cursor::new(&mut png_bytes),
            img_w as u32,
            img_h as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header write failed");
        writer
            .write_image_data(&pixels)
            .expect("PNG data write failed");
    }

    png_bytes
}

/// Async wrapper: render disk listing image on a blocking thread
pub async fn render_disk_listing_image_async(disk_info: DiskInfo) -> Vec<u8> {
    tokio::task::spawn_blocking(move || render_disk_listing_image(&disk_info))
        .await
        .unwrap_or_default()
}

/// Render a C64-style directory listing into a PNG image.
///
/// Returns raw PNG bytes that can be used as an iced image handle.
/// The image uses authentic C64 colours: blue background, light blue border,
/// white text — matching the real C64 directory listing appearance.
/// Check if a file is a previewable text file
pub fn is_text_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    matches!(
        ext.as_deref(),
        Some("txt") | Some("atxt") | Some("nfo") | Some("diz") | Some("readme")
    ) || path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .map(|s| s.starts_with("readme") || s == "file_id.diz")
        .unwrap_or(false)
}

/// Check if a file is a previewable image file
pub fn is_image_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    matches!(
        ext.as_deref(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp")
    )
}

/// Load text file content
pub fn load_text_file(path: &Path) -> Result<ContentPreview, String> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    // For PETSCII text files (.atxt), read as binary and convert
    let content = if ext.as_deref() == Some("atxt") {
        let bytes = std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
        petscii::convert_text_file(&bytes)
    } else {
        // Regular text file - try to read as UTF-8, fall back to lossy conversion
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => {
                // File might have non-UTF-8 bytes, read as binary and convert
                let bytes =
                    std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
                String::from_utf8_lossy(&bytes).to_string()
            }
        }
    };

    let line_count = content.lines().count();

    Ok(ContentPreview::Text {
        filename,
        content,
        line_count,
    })
}

/// Load image file
pub fn load_image_file(path: &Path) -> Result<ContentPreview, String> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image: {}", e))?;

    // Decode image to get dimensions
    let img =
        image::load_from_memory(&data).map_err(|e| format!("Failed to decode image: {}", e))?;

    let width = img.width();
    let height = img.height();

    Ok(ContentPreview::Image {
        filename,
        data,
        width,
        height,
    })
}

/// Async wrapper for loading text file
pub async fn load_text_file_async(path: PathBuf) -> Result<ContentPreview, String> {
    tokio::task::spawn_blocking(move || load_text_file(&path))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

/// Async wrapper for loading image file
pub async fn load_image_file_async(path: PathBuf) -> Result<ContentPreview, String> {
    tokio::task::spawn_blocking(move || load_image_file(&path))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}
