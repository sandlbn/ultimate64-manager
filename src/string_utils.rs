use std::path::Path;

/// Read a null-terminated ASCII string from binary data, filtering non-printable chars.
///
/// Reads from `data[offset..offset+max_len]`, stopping at the first null byte.
/// Only printable ASCII characters (32..127) are kept, and the result is trimmed.
pub fn read_binary_string(data: &[u8], offset: usize, max_len: usize) -> String {
    let s = &data[offset..offset + max_len];
    let end = s.iter().position(|&b| b == 0).unwrap_or(max_len);
    s[..end]
        .iter()
        .filter_map(|&b| {
            if b >= 32 && b < 127 {
                Some(b as char)
            } else {
                None
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
pub fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Truncate a path display string, showing "..." prefix if too long.
pub fn truncate_path(path: &Path, max_len: usize) -> String {
    let s = path.to_string_lossy();
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("...{}", &s[s.len().saturating_sub(max_len - 3)..])
    }
}
