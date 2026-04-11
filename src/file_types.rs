use std::path::Path;

// ─── Sort types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

impl SortOrder {
    pub fn toggle(self) -> Self {
        match self {
            SortOrder::Ascending => SortOrder::Descending,
            SortOrder::Descending => SortOrder::Ascending,
        }
    }

    pub fn indicator(self) -> &'static str {
        match self {
            SortOrder::Ascending => " \u{25B2}",
            SortOrder::Descending => " \u{25BC}",
        }
    }
}

// ─── File size formatting ────────────────────────────────────────────────────

/// Format a byte count as a human-readable size string
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ─── File type classification ────────────────────────────────────────────────

/// Check if an extension (lowercase, no dot) is a supported disk image format
pub fn is_disk_image(ext: &str) -> bool {
    matches!(ext, "d64" | "d71" | "d81" | "g64" | "g71" | "g81")
}

/// Check if a file path has a supported disk image extension
pub fn is_disk_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| is_disk_image(&s.to_lowercase()))
        .unwrap_or(false)
}

/// Check if an extension is a tape format
pub fn is_tape_file(ext: &str) -> bool {
    matches!(ext, "tap" | "t64")
}

/// Check if an extension is a program file
pub fn is_program_file(ext: &str) -> bool {
    matches!(ext, "prg" | "p00" | "seq" | "usr" | "rel")
}

/// Check if an extension is a music/audio file supported by the device
pub fn is_music_file(ext: &str) -> bool {
    matches!(ext, "sid" | "mod" | "xm" | "s3m")
}

/// Check if an extension is a REU (RAM Expansion Unit) image
pub fn is_reu_file(ext: &str) -> bool {
    ext == "reu"
}

/// Check if an extension is a ROM/binary file (custom kernal, char ROM, etc.)
pub fn is_rom_file(ext: &str) -> bool {
    matches!(ext, "rom" | "bin")
}

/// Check if an extension is an Ultimate config file
pub fn is_config_file(ext: &str) -> bool {
    ext == "cfg"
}

/// Check if an extension is an Ultimate firmware update file.
///
/// Covers all device variants:
/// - `.u2l` — Ultimate-II Lite
/// - `.u2p` — Ultimate-II+
/// - `.u2r` — Ultimate-II+ (ROM variant)
/// - `.u64` — Ultimate 64
/// - `.ue2` — Ultimate 64 Elite II
pub fn is_update_file(ext: &str) -> bool {
    matches!(ext, "u2l" | "u2p" | "u2r" | "u64" | "ue2")
}

/// Check if a file extension is supported for copying to the Ultimate device.
/// This is the master "is this a device-relevant file?" check.
pub fn is_device_file(ext: &str) -> bool {
    is_disk_image(ext)
        || is_tape_file(ext)
        || is_program_file(ext)
        || is_music_file(ext)
        || is_reu_file(ext)
        || is_rom_file(ext)
        || is_config_file(ext)
        || is_update_file(ext)
        || is_zip_file(ext)
        || ext == "crt"
}

/// Check if a filename (by name or path) is a previewable text file
pub fn is_text_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".txt")
        || lower.ends_with(".atxt")
        || lower.ends_with(".nfo")
        || lower.ends_with(".diz")
        || lower.starts_with("readme")
        || lower == "file_id.diz"
}

/// Check if a path is a previewable text file
pub fn is_text_file_path(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        is_text_file(name)
    } else {
        false
    }
}

/// Check if a filename is a previewable image file
pub fn is_image_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".bmp")
}

/// Check if a path is a previewable image file
pub fn is_image_file_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| {
            let lower = s.to_lowercase();
            matches!(lower.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp")
        })
        .unwrap_or(false)
}

/// Check if a filename is a PDF file
pub fn is_pdf_file(name: &str) -> bool {
    name.to_lowercase().ends_with(".pdf")
}

/// Check if a file extension indicates a ZIP archive
pub fn is_zip_file(ext: &str) -> bool {
    ext == "zip"
}

/// Check if a file extension indicates a runnable C64 file
pub fn is_runnable(ext: &str) -> bool {
    matches!(
        ext,
        "prg" | "crt" | "sid" | "d64" | "d71" | "d81" | "g64" | "g71" | "g81"
    )
}

/// Get a short file-type label for display (e.g., "PRG", "DSK", "SID")
pub fn get_file_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".prg")
        || lower.ends_with(".p00")
        || lower.ends_with(".seq")
        || lower.ends_with(".usr")
        || lower.ends_with(".rel")
    {
        "PRG"
    } else if lower.ends_with(".d64")
        || lower.ends_with(".g64")
        || lower.ends_with(".d71")
        || lower.ends_with(".g71")
        || lower.ends_with(".d81")
        || lower.ends_with(".g81")
    {
        "DSK"
    } else if lower.ends_with(".crt") {
        "CRT"
    } else if lower.ends_with(".sid") {
        "SID"
    } else if lower.ends_with(".mod") || lower.ends_with(".xm") || lower.ends_with(".s3m") {
        "MOD"
    } else if lower.ends_with(".tap") || lower.ends_with(".t64") {
        "TAP"
    } else if lower.ends_with(".reu") {
        "REU"
    } else if lower.ends_with(".rom") || lower.ends_with(".bin") {
        "ROM"
    } else if lower.ends_with(".cfg") {
        "CFG"
    } else if lower.ends_with(".u2l")
        || lower.ends_with(".u2p")
        || lower.ends_with(".u2r")
        || lower.ends_with(".u64")
        || lower.ends_with(".ue2")
    {
        "UPD"
    } else if is_pdf_file(&lower) {
        "PDF"
    } else if is_text_file(&lower) {
        "TXT"
    } else if is_image_file(&lower) {
        "IMG"
    } else {
        ""
    }
}

/// Get the display color for a file extension in the CSDb browser
pub fn ext_color(ext: &str) -> iced::Color {
    match ext {
        "prg" | "p00" | "seq" | "usr" | "rel" => iced::Color::from_rgb(0.5, 0.8, 0.5),
        "d64" | "d71" | "d81" | "g64" | "g71" | "g81" => iced::Color::from_rgb(0.5, 0.7, 0.9),
        "crt" => iced::Color::from_rgb(0.9, 0.7, 0.5),
        "sid" => iced::Color::from_rgb(0.8, 0.5, 0.8),
        "mod" | "xm" | "s3m" => iced::Color::from_rgb(0.7, 0.5, 0.9),
        "tap" | "t64" => iced::Color::from_rgb(0.8, 0.6, 0.4),
        "zip" => iced::Color::from_rgb(0.9, 0.9, 0.5),
        "reu" => iced::Color::from_rgb(0.6, 0.8, 0.8),
        "rom" | "bin" => iced::Color::from_rgb(0.7, 0.7, 0.5),
        "cfg" => iced::Color::from_rgb(0.6, 0.7, 0.8),
        "u2l" | "u2p" | "u2r" | "u64" | "ue2" => iced::Color::from_rgb(0.95, 0.55, 0.55),
        _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
    }
}
