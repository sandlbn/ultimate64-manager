//! Directory content preview module
//! Handles loading and displaying text files (readme, .txt, .atxt, .nfo, .diz)
//! and image files (.png, .jpg, .jpeg, .gif, .bmp) from the file browser.

use std::path::{Path, PathBuf};

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
