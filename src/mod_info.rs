//! Amiga MOD file parser
//! Extracts module name, author (if found), and calculates approximate duration

/// MOD file information
#[derive(Debug, Clone)]
pub struct ModInfo {
    pub name: String,
    pub author: Option<String>,
    pub duration_seconds: u32,
}

// MOD file structure offsets
const MOD_NAME_LENGTH: usize = 20;
const SAMPLE_HEADER_START: usize = 20;
const SAMPLE_HEADER_SIZE: usize = 30;
const SAMPLE_NAME_LENGTH: usize = 22;
const NUM_SAMPLES: usize = 31;
const SONG_LENGTH_OFFSET: usize = 950;
const PATTERN_TABLE_OFFSET: usize = 952;
const FORMAT_ID_OFFSET: usize = 1080;

/// Parse MOD file and extract information
pub fn parse_mod(data: &[u8]) -> Result<ModInfo, String> {
    if data.len() < 1084 {
        return Err("File too small to be a valid MOD".to_string());
    }

    // Read module name (first 20 bytes)
    let name = read_string(&data[0..MOD_NAME_LENGTH]);

    // Detect format and number of channels
    let format_id = &data[FORMAT_ID_OFFSET..FORMAT_ID_OFFSET + 4];
    let num_channels = detect_channels(format_id);

    // Song length (number of positions in pattern order)
    let song_length = data[SONG_LENGTH_OFFSET];

    // Find highest pattern number to know how many patterns exist
    let pattern_table = &data[PATTERN_TABLE_OFFSET..PATTERN_TABLE_OFFSET + 128];
    let num_patterns = pattern_table
        .iter()
        .take(song_length as usize)
        .max()
        .map(|&p| p + 1)
        .unwrap_or(0);

    // Try to find author in sample names
    let author = find_author(data);

    // Calculate duration by parsing pattern data for speed/tempo commands
    let duration_seconds = calculate_duration(data, song_length, num_patterns, num_channels);

    Ok(ModInfo {
        name,
        author,
        duration_seconds,
    })
}

/// Read null-terminated ASCII string, filtering non-printable chars
fn read_string(data: &[u8]) -> String {
    data.iter()
        .take_while(|&&b| b != 0)
        .filter(|&&b| b >= 32 && b < 127)
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Detect number of channels from format identifier
fn detect_channels(format_id: &[u8]) -> u8 {
    match format_id {
        b"M.K." | b"M!K!" | b"FLT4" | b"4CHN" => 4,
        b"6CHN" => 6,
        b"8CHN" | b"FLT8" | b"OCTA" => 8,
        b"CD81" | b"TDZ1" => 1,
        b"2CHN" | b"TDZ2" => 2,
        b"TDZ3" => 3,
        _ => {
            // Check for xxCH or xxCN pattern (e.g., "10CH", "16CN")
            if (format_id[2] == b'C' && (format_id[3] == b'H' || format_id[3] == b'N'))
                || (format_id[2] == b'C' && format_id[3] == b'H')
            {
                let tens = (format_id[0] as char).to_digit(10).unwrap_or(0);
                let ones = (format_id[1] as char).to_digit(10).unwrap_or(0);
                return (tens * 10 + ones) as u8;
            }
            4 // Default to 4 channels
        }
    }
}

/// Try to find author in sample names
fn find_author(data: &[u8]) -> Option<String> {
    // Check first few sample names for author info
    for i in 0..NUM_SAMPLES.min(5) {
        let offset = SAMPLE_HEADER_START + i * SAMPLE_HEADER_SIZE;
        if offset + SAMPLE_NAME_LENGTH > data.len() {
            break;
        }

        let sample_name = read_string(&data[offset..offset + SAMPLE_NAME_LENGTH]);

        if let Some(author) = extract_author(&sample_name) {
            return Some(author);
        }
    }

    // Also check module name
    let mod_name = read_string(&data[0..MOD_NAME_LENGTH]);
    extract_author(&mod_name)
}

/// Extract author from text using common patterns
fn extract_author(text: &str) -> Option<String> {
    let lower = text.to_lowercase();

    // Pattern: "by author"
    if let Some(pos) = lower.find("by ") {
        let author = text[pos + 3..].trim();
        if author.len() >= 2 {
            return Some(author.to_string());
        }
    }

    // Pattern: "/author" or "- author"
    for sep in ['/', '-', '|'].iter() {
        if let Some(pos) = text.rfind(*sep) {
            let author = text[pos + 1..].trim();
            if author.len() >= 2 && !author.chars().all(|c| c.is_numeric()) {
                return Some(author.to_string());
            }
        }
    }

    // Pattern: "(author)"
    if let (Some(start), Some(end)) = (text.find('('), text.find(')')) {
        if end > start + 2 {
            let author = text[start + 1..end].trim();
            if !author.is_empty() {
                return Some(author.to_string());
            }
        }
    }

    None
}

/// Calculate approximate song duration by parsing pattern data
fn calculate_duration(data: &[u8], song_length: u8, num_patterns: u8, num_channels: u8) -> u32 {
    // Pattern data layout:
    // - Each pattern has 64 rows
    // - Each row has `num_channels` notes
    // - Each note is 4 bytes
    let channels = num_channels as usize;
    let pattern_size = 64 * channels * 4;
    let pattern_data_start = 1084;

    // Default speed (ticks per row) and tempo (BPM)
    let mut speed: u32 = 6;
    let mut tempo: u32 = 125;

    // Track total rows and handle pattern breaks
    let mut total_rows: u32 = 0;
    let pattern_table = &data[PATTERN_TABLE_OFFSET..PATTERN_TABLE_OFFSET + 128];

    // Process each position in the song
    for pos in 0..song_length as usize {
        let pattern_num = pattern_table[pos] as usize;
        if pattern_num >= num_patterns as usize {
            total_rows += 64; // Assume full pattern if invalid
            continue;
        }

        let pattern_offset = pattern_data_start + pattern_num * pattern_size;
        if pattern_offset + pattern_size > data.len() {
            total_rows += 64;
            continue;
        }

        // Scan pattern for effect commands
        let mut rows_in_pattern = 64u32;

        'row_loop: for row in 0..64 {
            for channel in 0..channels {
                let note_offset = pattern_offset + (row * channels * 4) + (channel * 4);
                if note_offset + 3 >= data.len() {
                    continue;
                }

                // Note format: [sample_hi:4 | period_hi:4] [period_lo:8] [sample_lo:4 | effect:4] [param:8]
                let effect = data[note_offset + 2] & 0x0F;
                let param = data[note_offset + 3];

                match effect {
                    // Effect Fxx: Set speed/tempo
                    0x0F => {
                        if param > 0 && param < 0x20 {
                            speed = param as u32;
                        } else if param >= 0x20 {
                            tempo = param as u32;
                        }
                    }
                    // Effect Dxx: Pattern break
                    0x0D => {
                        rows_in_pattern = row as u32 + 1;
                        break 'row_loop;
                    }
                    // Effect Bxx: Position jump (we'll just count remaining rows)
                    0x0B => {
                        rows_in_pattern = row as u32 + 1;
                        break 'row_loop;
                    }
                    _ => {}
                }
            }
        }

        total_rows += rows_in_pattern;
    }

    // Calculate duration:
    // - Each row takes `speed` ticks
    // - Tick duration = 2.5 / tempo seconds
    // - Row duration = speed * 2.5 / tempo seconds
    let row_duration_ms = (speed as f64 * 2500.0 / tempo as f64) as u32;
    let duration_ms = total_rows * row_duration_ms;

    // Return duration in seconds, minimum 1 second
    (duration_ms / 1000).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_string() {
        let data = b"Hello World\0\0\0\0\0";
        assert_eq!(read_string(data), "Hello World");
    }

    #[test]
    fn test_extract_author() {
        assert_eq!(
            extract_author("Cool Song by Jochen"),
            Some("Jochen".to_string())
        );
        assert_eq!(
            extract_author("mysong / Purple Motion"),
            Some("Purple Motion".to_string())
        );
        assert_eq!(extract_author("test - Skaven"), Some("Skaven".to_string()));
        assert_eq!(
            extract_author("(Lizardking)"),
            Some("Lizardking".to_string())
        );
    }

    #[test]
    fn test_detect_channels() {
        assert_eq!(detect_channels(b"M.K."), 4);
        assert_eq!(detect_channels(b"8CHN"), 8);
        assert_eq!(detect_channels(b"6CHN"), 6);
    }
}
