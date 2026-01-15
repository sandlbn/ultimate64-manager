//! PETSCII character conversion module
//!
//! Provides functionality to convert between PETSCII (Commodore character encoding)
//! and Unicode/ASCII for display purposes.

/// Convert PETSCII bytes to a displayable string
/// Handles padding characters
pub fn to_string(bytes: &[u8]) -> String {
    let mut result = String::new();

    for &b in bytes {
        // Skip $A0 padding (PETSCII shifted space used for padding)
        if b == 0xA0 {
            continue;
        }

        let ch = byte_to_char(b);
        result.push(ch);
    }

    result.trim_end().to_string()
}

/// Convert a single PETSCII byte to a displayable character
/// Uses direct mapping for reliable cross-platform behavior
pub fn byte_to_char(petscii_code: u8) -> char {
    match petscii_code {
        // Control characters
        0x00..=0x1F => ' ',
        // Space
        0x20 => ' ',
        // Numbers and symbols (same as ASCII)
        0x21..=0x3F => petscii_code as char,
        // @ symbol
        0x40 => '@',
        // Uppercase A-Z (PETSCII standard)
        0x41..=0x5A => petscii_code as char,
        // Brackets and special chars
        0x5B => '[',
        0x5C => '£',
        0x5D => ']',
        0x5E => '^', // Up arrow, use caret as ASCII substitute
        0x5F => '_', // Left arrow, use underscore as ASCII substitute
        // Horizontal line
        0x60 => '-',
        // Lowercase a-z (shifted mode)
        0x61..=0x7A => petscii_code as char,
        // Graphics characters
        0x7B..=0x7F => '#',
        0x80..=0x9F => '#',
        // Shifted space (padding)
        0xA0 => ' ',
        // Graphics
        0xA1..=0xBF => '#',
        // Horizontal line
        0xC0 => '-',
        // Uppercase A-Z again (alternate range)
        0xC1..=0xDA => ((petscii_code - 0xC1) + b'A') as char,
        // More graphics
        0xDB..=0xFF => '#',
    }
}

/// Convert a PETSCII/Commodore text file content to Unicode
/// Handles control characters and line endings
pub fn convert_text_file(input: &[u8]) -> String {
    let mut result = String::new();

    for &b in input {
        let ch = match b {
            // Line endings
            0x0d => '\n', // CR to newline (PETSCII line ending)
            0x0a => '\n', // LF (in case of mixed line endings)

            // Control characters to skip/ignore
            0x00 => continue,        // Null
            0x01..=0x09 => continue, // Color codes and control (includes tab, C=+Shift)
            0x0e => continue,        // Switch to lowercase
            0x11..=0x14 => continue, // Cursor down, reverse on, home, delete
            0x1c..=0x1f => continue, // Color codes and cursor right
            0x81..=0x9f => continue, // Control codes (cursor, reverse off, colors, uppercase switch)

            // Regular characters - use standard conversion
            _ => byte_to_char(b),
        };
        result.push(ch);
    }

    result
}

/// Convert a string slice that may contain raw PETSCII bytes read as Latin-1/ISO-8859-1
/// This is useful when files are read with std::fs::read_to_string which interprets
/// bytes as UTF-8 or when the file contains mixed ASCII and PETSCII
#[allow(dead_code)]
pub fn convert_mixed_text(input: &str) -> String {
    // Convert string to bytes and process
    convert_text_file(input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_to_char_uppercase() {
        assert_eq!(byte_to_char(0x41), 'A');
        assert_eq!(byte_to_char(0x5A), 'Z');
    }

    #[test]
    fn test_byte_to_char_numbers() {
        assert_eq!(byte_to_char(0x30), '0');
        assert_eq!(byte_to_char(0x39), '9');
    }

    #[test]
    fn test_byte_to_char_space() {
        assert_eq!(byte_to_char(0x20), ' ');
        assert_eq!(byte_to_char(0xA0), ' ');
    }

    #[test]
    fn test_to_string_with_padding() {
        // "TEST" followed by $A0 padding
        let bytes = [0x54, 0x45, 0x53, 0x54, 0xA0, 0xA0, 0xA0];
        assert_eq!(to_string(&bytes), "TEST");
    }

    #[test]
    fn test_convert_text_file_linebreak() {
        let bytes = [0x48, 0x49, 0x0D, 0x42, 0x59, 0x45]; // "HI\rBYE"
        let result = convert_text_file(&bytes);
        assert!(result.contains('\n'));
    }
}
