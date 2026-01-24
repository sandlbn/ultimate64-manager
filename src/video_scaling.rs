use crate::streaming::{VIC_HEIGHT, VIC_WIDTH};

// C64 color palette (RGB values) - from u64view
pub const C64_PALETTE: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // 0: Black
    [0xFF, 0xFF, 0xFF], // 1: White
    [0x68, 0x37, 0x2B], // 2: Red
    [0x70, 0xA4, 0xB2], // 3: Cyan
    [0x6F, 0x3D, 0x86], // 4: Purple
    [0x58, 0x8D, 0x43], // 5: Green
    [0x35, 0x28, 0x79], // 6: Blue
    [0xB8, 0xC7, 0x6F], // 7: Yellow
    [0x6F, 0x4F, 0x25], // 8: Orange
    [0x43, 0x39, 0x00], // 9: Brown
    [0x9A, 0x67, 0x59], // 10: Light Red
    [0x44, 0x44, 0x44], // 11: Dark Grey
    [0x6C, 0x6C, 0x6C], // 12: Grey
    [0x9A, 0xD2, 0x84], // 13: Light Green
    [0x6C, 0x5E, 0xB5], // 14: Light Blue
    [0x95, 0x95, 0x95], // 15: Light Grey
];

/// Scale2x (EPX) algorithm - smooths edges while preserving sharp details
/// Input: RGBA buffer at original size
/// Output: RGBA buffer at 2x size
pub fn scale2x(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let out_w = w * 2;
    let out_h = h * 2;
    let mut output = vec![0u8; out_w * out_h * 4];

    for y in 0..h {
        for x in 0..w {
            // Get center pixel P and neighbors A,B,C,D (with edge clamping):
            //     A
            //   C P B
            //     D
            let p = get_pixel(input, w, h, x, y);
            let a = get_pixel(input, w, h, x, y.saturating_sub(1)); // Top
            let b = get_pixel(input, w, h, x.saturating_add(1).min(w - 1), y); // Right
            let c = get_pixel(input, w, h, x.saturating_sub(1), y); // Left
            let d = get_pixel(input, w, h, x, y.saturating_add(1).min(h - 1)); // Bottom

            // Scale2x rules:
            // If A==C and A!=B and C!=D -> output[0] = A, else P
            // If A==B and A!=C and B!=D -> output[1] = B, else P
            // If C==D and A!=C and B!=D -> output[2] = C, else P
            // If B==D and A!=B and C!=D -> output[3] = D, else P

            let p0 = if colors_equal(&a, &c) && !colors_equal(&a, &b) && !colors_equal(&c, &d) {
                a
            } else {
                p
            };
            let p1 = if colors_equal(&a, &b) && !colors_equal(&a, &c) && !colors_equal(&b, &d) {
                b
            } else {
                p
            };
            let p2 = if colors_equal(&c, &d) && !colors_equal(&a, &c) && !colors_equal(&b, &d) {
                c
            } else {
                p
            };
            let p3 = if colors_equal(&b, &d) && !colors_equal(&a, &b) && !colors_equal(&c, &d) {
                d
            } else {
                p
            };

            // Write 2x2 output pixels:
            // p0 | p1   (top-left | top-right)
            // ---+---
            // p2 | p3   (bottom-left | bottom-right)
            let out_x = x * 2;
            let out_y = y * 2;
            set_pixel(&mut output, out_w, out_x, out_y, &p0); // top-left
            set_pixel(&mut output, out_w, out_x + 1, out_y, &p1); // top-right
            set_pixel(&mut output, out_w, out_x, out_y + 1, &p2); // bottom-left
            set_pixel(&mut output, out_w, out_x + 1, out_y + 1, &p3); // bottom-right
        }
    }

    output
}

/// Apply CRT-style scanlines effect
/// Input: RGBA buffer at original size
/// Output: RGBA buffer at 2x size with darkened even lines
pub fn apply_scanlines(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let out_w = w * 2;
    let out_h = h * 2;
    let mut output = vec![0u8; out_w * out_h * 4];

    // Scanline intensity (0.0 = black lines, 1.0 = no effect)
    let scanline_brightness: f32 = 0.55;

    for y in 0..h {
        for x in 0..w {
            let pixel = get_pixel(input, w, h, x, y);

            // Create darkened version for scanlines
            let dark_pixel = [
                (pixel[0] as f32 * scanline_brightness) as u8,
                (pixel[1] as f32 * scanline_brightness) as u8,
                (pixel[2] as f32 * scanline_brightness) as u8,
                pixel[3],
            ];

            // Write 2x2 output: top row normal, bottom row darkened
            let out_x = x * 2;
            let out_y = y * 2;

            // Top row - full brightness (duplicated horizontally)
            set_pixel(&mut output, out_w, out_x, out_y, &pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y, &pixel);

            // Bottom row - darkened (scanline effect)
            set_pixel(&mut output, out_w, out_x, out_y + 1, &dark_pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y + 1, &dark_pixel);
        }
    }

    output
}

/// Get pixel from RGBA buffer with bounds checking
#[inline]
pub fn get_pixel(data: &[u8], width: usize, height: usize, x: usize, y: usize) -> [u8; 4] {
    if x >= width || y >= height {
        return [0, 0, 0, 255];
    }
    let idx = (y * width + x) * 4;
    if idx + 3 < data.len() {
        [data[idx], data[idx + 1], data[idx + 2], data[idx + 3]]
    } else {
        [0, 0, 0, 255]
    }
}

/// Set pixel in RGBA buffer
#[inline]
fn set_pixel(data: &mut [u8], width: usize, x: usize, y: usize, pixel: &[u8; 4]) {
    let idx = (y * width + x) * 4;
    if idx + 3 < data.len() {
        data[idx] = pixel[0];
        data[idx + 1] = pixel[1];
        data[idx + 2] = pixel[2];
        data[idx + 3] = pixel[3];
    }
}

/// Compare two pixels for equality (RGB only, ignore alpha)
#[inline]
pub fn colors_equal(a: &[u8; 4], b: &[u8; 4]) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}

// Decode VIC stream frame to RGBA (used for raw frame data, not packet-based data)
#[allow(dead_code)]
pub fn decode_vic_frame(raw_data: &[u8]) -> Option<Vec<u8>> {
    let expected_indexed = (VIC_WIDTH * VIC_HEIGHT) as usize; // 104448 bytes (1 byte/pixel)
    let expected_rgb = expected_indexed * 3; // 313344 bytes (3 bytes/pixel)
    let expected_rgba = expected_indexed * 4; // 417792 bytes (4 bytes/pixel)

    log::debug!("Decoding frame: {} bytes", raw_data.len());

    if raw_data.len() == expected_indexed {
        // Indexed color mode - convert using C64 palette
        let mut rgba = Vec::with_capacity(expected_rgba);
        for &pixel in raw_data {
            let idx = (pixel & 0x0F) as usize;
            let color = &C64_PALETTE[idx];
            rgba.push(color[0]);
            rgba.push(color[1]);
            rgba.push(color[2]);
            rgba.push(255);
        }
        Some(rgba)
    } else if raw_data.len() == expected_rgb {
        // RGB mode - convert to RGBA
        let mut rgba = Vec::with_capacity(expected_rgba);
        for chunk in raw_data.chunks(3) {
            if chunk.len() == 3 {
                rgba.push(chunk[0]);
                rgba.push(chunk[1]);
                rgba.push(chunk[2]);
                rgba.push(255);
            }
        }
        Some(rgba)
    } else if raw_data.len() == expected_rgba {
        // Already RGBA
        Some(raw_data.to_vec())
    } else if raw_data.len() >= expected_indexed {
        // Unknown format but has enough data for indexed mode.
        // This is a best-effort fallback: interpret first 104448 bytes as
        // indexed color data (1 byte per pixel, color index 0-15).
        let mut rgba = Vec::with_capacity(expected_rgba);
        for &pixel in raw_data.iter().take(expected_indexed) {
            let idx = (pixel & 0x0F) as usize;
            let color = &C64_PALETTE[idx];
            rgba.push(color[0]);
            rgba.push(color[1]);
            rgba.push(color[2]);
            rgba.push(255);
        }
        Some(rgba)
    } else {
        log::warn!(
            "Unknown frame format: {} bytes (expected {} or {} or {})",
            raw_data.len(),
            expected_indexed,
            expected_rgb,
            expected_rgba
        );
        None
    }
}

/// CRT effect
pub fn apply_crt_effect(input: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let out_w = w * 2;
    let out_h = h * 2;
    let mut output = vec![0u8; out_w * out_h * 4];

    let scanline_bright: f32 = 0.5;

    for y in 0..h {
        for x in 0..w {
            let pixel = get_pixel(input, w, h, x, y);
            let r = pixel[0] as f32;
            let g = pixel[1] as f32;
            let b = pixel[2] as f32;

            // Shadow mask pattern - alternates based on x position
            // This simulates the RGB phosphor arrangement
            let phase = x % 3;

            let (r_mult, g_mult, b_mult) = match phase {
                0 => (1.0, 0.85, 0.85), // Emphasize red
                1 => (0.85, 1.0, 0.85), // Emphasize green
                _ => (0.85, 0.85, 1.0), // Emphasize blue
            };

            // Apply bloom to bright pixels
            let bloom = 1.1;
            let bright_pixel = [
                ((r * r_mult * bloom).min(255.0)) as u8,
                ((g * g_mult * bloom).min(255.0)) as u8,
                ((b * b_mult * bloom).min(255.0)) as u8,
                255,
            ];

            // Scanline version (darker)
            let dark_pixel = [
                ((r * r_mult * scanline_bright).min(255.0)) as u8,
                ((g * g_mult * scanline_bright).min(255.0)) as u8,
                ((b * b_mult * scanline_bright).min(255.0)) as u8,
                255,
            ];

            let out_x = x * 2;
            let out_y = y * 2;

            // Top row - bright with shadow mask tint
            set_pixel(&mut output, out_w, out_x, out_y, &bright_pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y, &bright_pixel);

            // Bottom row - scanline (darker)
            set_pixel(&mut output, out_w, out_x, out_y + 1, &dark_pixel);
            set_pixel(&mut output, out_w, out_x + 1, out_y + 1, &dark_pixel);
        }
    }

    output
}
/// Integer scaling - perfect pixel replication with no filtering
/// Fast integer scaling - copies entire rows at once
pub fn integer_scale(input: &[u8], width: u32, height: u32, scale: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let s = scale as usize;
    let out_w = w * s;
    let out_h = h * s;
    let mut output = vec![0u8; out_w * out_h * 4];

    for y in 0..h {
        let src_row_start = y * w * 4;

        // Build one scaled row
        let mut scaled_row = Vec::with_capacity(out_w * 4);
        for x in 0..w {
            let src_idx = src_row_start + x * 4;
            // Repeat each pixel 'scale' times horizontally
            for _ in 0..s {
                scaled_row.extend_from_slice(&input[src_idx..src_idx + 4]);
            }
        }

        // Copy the scaled row 'scale' times vertically
        for dy in 0..s {
            let out_row_start = (y * s + dy) * out_w * 4;
            output[out_row_start..out_row_start + scaled_row.len()].copy_from_slice(&scaled_row);
        }
    }

    output
}
