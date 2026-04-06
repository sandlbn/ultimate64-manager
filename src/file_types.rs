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
    matches!(ext, "d64" | "d71" | "d81" | "g64" | "g71")
}

/// Check if a file path has a supported disk image extension
pub fn is_disk_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| is_disk_image(&s.to_lowercase()))
        .unwrap_or(false)
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
    matches!(ext, "prg" | "crt" | "sid" | "d64" | "d71" | "d81" | "g64")
}

/// Get a short file-type label for display (e.g., "PRG", "DSK", "SID")
pub fn get_file_icon(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".prg") {
        "PRG"
    } else if lower.ends_with(".d64")
        || lower.ends_with(".g64")
        || lower.ends_with(".d71")
        || lower.ends_with(".g71")
        || lower.ends_with(".d81")
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
        "prg" => iced::Color::from_rgb(0.5, 0.8, 0.5),
        "d64" | "d71" | "d81" | "g64" => iced::Color::from_rgb(0.5, 0.7, 0.9),
        "crt" => iced::Color::from_rgb(0.9, 0.7, 0.5),
        "sid" => iced::Color::from_rgb(0.8, 0.5, 0.8),
        "zip" => iced::Color::from_rgb(0.9, 0.9, 0.5),
        _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
    }
}
