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

/// Truncate a single string in the middle, keeping both ends joined by a `…`.
///
/// Used when even a bare filename is longer than the available budget: the
/// distinguishing head and tail both stay visible (e.g. `Turrican_III…part_2`
/// rather than dropping either end).
pub fn middle_truncate(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let keep = max_chars - 1; // one column for the ellipsis
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let chars: Vec<char> = s.chars().collect();
    let start: String = chars[..head].iter().collect();
    let end: String = chars[total - tail..].iter().collect();
    format!("{}…{}", start, end)
}

/// Fit a relative path (`dir/sub/filename`) into `max_chars`, always keeping the
/// **filename fully visible**. When the whole string is too long, the leading
/// directory portion is elided with a `…` prefix; if even the filename alone
/// doesn't fit, the filename itself is middle-truncated.
///
/// This replaces blind end-truncation, which cut the filename off — the very
/// part that distinguishes otherwise-similar entries.
pub fn fit_path(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    // Last path segment, handling both separators.
    let file = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let file_len = file.chars().count();
    // Reserve one column for the leading ellipsis.
    if file_len + 1 >= max_chars {
        // Even the filename doesn't fit — keep its head and tail.
        return middle_truncate(file, max_chars);
    }
    // Keep the whole filename plus as much trailing path as fits.
    let keep = max_chars - 1; // room for the leading '…'
    let tail: String = s.chars().skip(total - keep).collect();
    format!("…{}", tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_path_keeps_full_filename_and_elides_leading_dirs() {
        let p = "MUSICIANS/C/Crosspider/Turrican_III_Demo_part_2.sid";
        let out = fit_path(p, 40);
        assert!(out.chars().count() <= 40);
        assert!(out.starts_with('…'));
        // The distinguishing filename survives in full.
        assert!(out.ends_with("Turrican_III_Demo_part_2.sid"), "got: {out}");
    }

    #[test]
    fn fit_path_returns_unchanged_when_it_fits() {
        let p = "sub/track.sid";
        assert_eq!(fit_path(p, 40), p);
    }

    #[test]
    fn fit_path_middle_truncates_an_overlong_filename() {
        // No room even for the filename: keep head + tail so both ends show.
        let name = "A_Really_Very_Long_Single_Filename_Without_Dirs.sid";
        let out = fit_path(name, 20);
        assert!(out.chars().count() <= 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("A_Really"));
        assert!(out.ends_with(".sid"), "got: {out}");
    }

    #[test]
    fn middle_truncate_keeps_both_ends() {
        assert_eq!(middle_truncate("abcdefghij", 10), "abcdefghij");
        let out = middle_truncate("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.contains('…'));
    }
}
