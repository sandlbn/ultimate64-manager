//! PDF preview module
//!
//! Provides functionality to preview PDF files:
//! - Extracts embedded images from scanned PDFs
//! - Falls back to text extraction for text-based PDFs
//!
//! Uses pure Rust libraries (lopdf, pdf-extract) - no external dependencies.

use crate::dir_preview::ContentPreview;
use lopdf::Document;
use std::path::Path;

/// Load PDF preview from file path
pub fn load_pdf_preview(path: &Path) -> Result<ContentPreview, String> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let data = std::fs::read(path).map_err(|e| format!("Failed to read PDF: {}", e))?;

    load_pdf_preview_from_bytes(&data, filename)
}

/// Load PDF preview from bytes
pub fn load_pdf_preview_from_bytes(
    data: &[u8],
    filename: String,
) -> Result<ContentPreview, String> {
    // First try to extract an embedded image (for scanned PDFs)
    if let Ok(preview) = extract_pdf_image(data, filename.clone()) {
        return Ok(preview);
    }

    // Fall back to text extraction
    extract_pdf_text(data, filename)
}

/// Try to extract embedded image from PDF (works for scanned documents)
fn extract_pdf_image(data: &[u8], filename: String) -> Result<ContentPreview, String> {
    let doc = Document::load_mem(data).map_err(|e| format!("Failed to parse PDF: {}", e))?;

    // Iterate through pages looking for images
    for page_id in doc.page_iter() {
        // Look for XObject references in page resources
        let (resources_opt, _) = doc.get_page_resources(page_id);
        if let Some(resources) = resources_opt {
            if let Ok(xobjects) = resources.get(b"XObject") {
                if let Ok(xobj_dict) = xobjects.as_dict() {
                    for (_name, obj_ref) in xobj_dict.iter() {
                        // obj_ref is an Object, need to get the reference ID
                        if let Ok(ref_id) = obj_ref.as_reference() {
                            if let Ok(stream) = doc.get_object(ref_id).and_then(|o| o.as_stream()) {
                                let dict = &stream.dict;

                                // Check if this is an image
                                if let Ok(subtype) = dict.get(b"Subtype") {
                                    if subtype.as_name_str().ok() == Some("Image") {
                                        // Try to extract the image
                                        if let Ok(preview) =
                                            extract_image_from_stream(stream, filename.clone())
                                        {
                                            return Ok(preview);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Err("No embedded images found".to_string())
}

/// Extract image data from a PDF stream
fn extract_image_from_stream(
    stream: &lopdf::Stream,
    filename: String,
) -> Result<ContentPreview, String> {
    let dict = &stream.dict;

    // Get image dimensions
    let width = dict
        .get(b"Width")
        .ok()
        .and_then(|w| w.as_i64().ok())
        .unwrap_or(0) as u32;
    let height = dict
        .get(b"Height")
        .ok()
        .and_then(|h| h.as_i64().ok())
        .unwrap_or(0) as u32;

    if width == 0 || height == 0 {
        return Err("Invalid image dimensions".to_string());
    }

    // Get the filter type
    let filter = dict
        .get(b"Filter")
        .ok()
        .and_then(|f| f.as_name_str().ok())
        .unwrap_or("");

    // Get color space
    let color_space = dict
        .get(b"ColorSpace")
        .ok()
        .and_then(|cs| cs.as_name_str().ok())
        .unwrap_or("DeviceGray");

    let bits_per_component = dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|b| b.as_i64().ok())
        .unwrap_or(8) as u8;

    // Decode the stream content
    let raw_data = stream
        .decompressed_content()
        .map_err(|e| format!("Failed to decompress: {}", e))?;

    // Handle different image formats
    match filter {
        "DCTDecode" => {
            // JPEG data - can use directly
            let img = image::load_from_memory_with_format(&raw_data, image::ImageFormat::Jpeg)
                .map_err(|e| format!("Failed to decode JPEG: {}", e))?;

            let mut png_data: Vec<u8> = Vec::new();
            img.write_to(
                &mut std::io::Cursor::new(&mut png_data),
                image::ImageFormat::Png,
            )
            .map_err(|e| format!("Failed to encode PNG: {}", e))?;

            Ok(ContentPreview::Image {
                filename,
                data: png_data,
                width: img.width(),
                height: img.height(),
            })
        }
        "FlateDecode" | "" => {
            // Raw pixel data - convert to image
            convert_raw_to_image(
                &raw_data,
                width,
                height,
                color_space,
                bits_per_component,
                filename,
            )
        }
        "JPXDecode" => {
            // JPEG2000 - try to decode
            let img = image::load_from_memory(&raw_data)
                .map_err(|e| format!("Failed to decode JPEG2000: {}", e))?;

            let mut png_data: Vec<u8> = Vec::new();
            img.write_to(
                &mut std::io::Cursor::new(&mut png_data),
                image::ImageFormat::Png,
            )
            .map_err(|e| format!("Failed to encode PNG: {}", e))?;

            Ok(ContentPreview::Image {
                filename,
                data: png_data,
                width: img.width(),
                height: img.height(),
            })
        }
        _ => Err(format!("Unsupported image filter: {}", filter)),
    }
}

/// Convert raw pixel data to an image
fn convert_raw_to_image(
    raw_data: &[u8],
    width: u32,
    height: u32,
    color_space: &str,
    bits_per_component: u8,
    filename: String,
) -> Result<ContentPreview, String> {
    let img = match (color_space, bits_per_component) {
        ("DeviceGray", 8) => image::GrayImage::from_raw(width, height, raw_data.to_vec())
            .map(image::DynamicImage::ImageLuma8)
            .ok_or_else(|| "Failed to create grayscale image".to_string())?,
        ("DeviceRGB", 8) => image::RgbImage::from_raw(width, height, raw_data.to_vec())
            .map(image::DynamicImage::ImageRgb8)
            .ok_or_else(|| "Failed to create RGB image".to_string())?,
        ("DeviceCMYK", 8) => {
            // Convert CMYK to RGB
            let rgb_data: Vec<u8> = raw_data
                .chunks(4)
                .flat_map(|cmyk| {
                    if cmyk.len() == 4 {
                        let c = cmyk[0] as f32 / 255.0;
                        let m = cmyk[1] as f32 / 255.0;
                        let y = cmyk[2] as f32 / 255.0;
                        let k = cmyk[3] as f32 / 255.0;
                        let r = ((1.0 - c) * (1.0 - k) * 255.0) as u8;
                        let g = ((1.0 - m) * (1.0 - k) * 255.0) as u8;
                        let b = ((1.0 - y) * (1.0 - k) * 255.0) as u8;
                        vec![r, g, b]
                    } else {
                        vec![0, 0, 0]
                    }
                })
                .collect();

            image::RgbImage::from_raw(width, height, rgb_data)
                .map(image::DynamicImage::ImageRgb8)
                .ok_or_else(|| "Failed to create RGB image from CMYK".to_string())?
        }
        ("DeviceGray", 1) => {
            // 1-bit grayscale (black and white)
            let gray_data: Vec<u8> = raw_data
                .iter()
                .flat_map(|byte| {
                    (0..8)
                        .rev()
                        .map(move |bit| if (byte >> bit) & 1 == 1 { 255u8 } else { 0u8 })
                })
                .take((width * height) as usize)
                .collect();

            image::GrayImage::from_raw(width, height, gray_data)
                .map(image::DynamicImage::ImageLuma8)
                .ok_or_else(|| "Failed to create 1-bit grayscale image".to_string())?
        }
        _ => {
            return Err(format!(
                "Unsupported color space/depth: {} / {} bits",
                color_space, bits_per_component
            ));
        }
    };

    let mut png_data: Vec<u8> = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_data),
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("Failed to encode PNG: {}", e))?;

    Ok(ContentPreview::Image {
        filename,
        data: png_data,
        width,
        height,
    })
}

/// Extract text from PDF as fallback
fn extract_pdf_text(data: &[u8], filename: String) -> Result<ContentPreview, String> {
    let content = pdf_extract::extract_text_from_mem(data)
        .map_err(|e| format!("Failed to extract text: {}", e))?;

    let content = content.trim().to_string();

    if content.is_empty() {
        return Err("PDF contains no extractable text or images".to_string());
    }

    let line_count = content.lines().count();

    Ok(ContentPreview::Text {
        filename,
        content,
        line_count,
    })
}

/// Async wrapper for loading PDF preview from path
pub async fn load_pdf_preview_async(path: std::path::PathBuf) -> Result<ContentPreview, String> {
    tokio::task::spawn_blocking(move || load_pdf_preview(&path))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

/// Async wrapper for loading PDF preview from bytes
pub async fn load_pdf_preview_from_bytes_async(
    data: Vec<u8>,
    filename: String,
) -> Result<ContentPreview, String> {
    tokio::task::spawn_blocking(move || load_pdf_preview_from_bytes(&data, filename))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}
