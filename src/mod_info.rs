//! Amiga MOD file parser based on: https://eblong.com/zarf/blorb/mod-spec.txt
//! TODO: Add S3M

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
///
/// Formula: row_duration = (2.5 * speed) / tempo seconds
/// Where: speed = ticks per row (default 6), tempo = BPM (default 125)
///
/// At defaults: row_duration = (2.5 * 6) / 125 = 0.12 seconds per row
fn calculate_duration(data: &[u8], song_length: u8, num_patterns: u8, num_channels: u8) -> u32 {
    let channels = num_channels as usize;
    let pattern_size = 64 * channels * 4;
    let pattern_data_start = 1084;

    // Default speed (ticks per row) and tempo (BPM)
    let mut speed: f64 = 6.0;
    let mut tempo: f64 = 125.0;

    // Track total time in milliseconds (use f64 for precision)
    let mut total_ms: f64 = 0.0;

    let pattern_table = &data[PATTERN_TABLE_OFFSET..PATTERN_TABLE_OFFSET + 128];

    // Track visited positions to detect infinite loops
    let mut visited_positions: std::collections::HashSet<(u8, u8)> =
        std::collections::HashSet::new();

    let mut pos: usize = 0;
    let mut safety_counter = 0;
    const MAX_ITERATIONS: usize = 10000; // Prevent infinite loops

    while pos < song_length as usize && safety_counter < MAX_ITERATIONS {
        safety_counter += 1;

        let pattern_num = pattern_table[pos] as usize;
        if pattern_num >= num_patterns as usize {
            // Invalid pattern, skip
            pos += 1;
            continue;
        }

        let pattern_offset = pattern_data_start + pattern_num * pattern_size;
        if pattern_offset + pattern_size > data.len() {
            pos += 1;
            continue;
        }

        // Process each row in the pattern
        let mut row: usize = 0;
        while row < 64 {
            // Check for infinite loop (same position + row visited twice)
            let state = (pos as u8, row as u8);
            if visited_positions.contains(&state) {
                // We've been here before - likely a loop, stop counting
                return (total_ms / 1000.0).max(1.0) as u32;
            }
            visited_positions.insert(state);

            // Calculate time for this row: (2.5 * speed / tempo) seconds
            let row_ms = (2500.0 * speed) / tempo;

            // Check for pattern delay (EEx effect) - multiplies row duration
            let mut pattern_delay: u32 = 0;

            // Scan all channels for effects
            for channel in 0..channels {
                let note_offset = pattern_offset + (row * channels * 4) + (channel * 4);
                if note_offset + 3 >= data.len() {
                    continue;
                }

                let effect = data[note_offset + 2] & 0x0F;
                let param = data[note_offset + 3];

                match effect {
                    // Effect Fxx: Set speed (01-1F) or tempo (20-FF)
                    0x0F => {
                        if param == 0 {
                            // F00 stops the song in some trackers, ignore
                        } else if param < 0x20 {
                            speed = param as f64;
                        } else {
                            tempo = param as f64;
                        }
                    }
                    // Effect Dxx: Pattern break - go to next pattern at row x*10+y (DECIMAL!)
                    0x0D => {
                        let break_row = ((param >> 4) * 10 + (param & 0x0F)) as usize;
                        // Add remaining row time
                        total_ms += row_ms;
                        // Jump to next position at specified row
                        pos += 1;
                        row = break_row.min(63);
                        continue;
                    }
                    // Effect Bxx: Position jump
                    0x0B => {
                        total_ms += row_ms;
                        let jump_pos = param as usize;
                        if jump_pos <= pos {
                            // Jumping backwards - likely a loop, stop here
                            return (total_ms / 1000.0).max(1.0) as u32;
                        }
                        pos = jump_pos;
                        row = 0;
                        continue;
                    }
                    // Effect EEx: Pattern delay - repeat row x times
                    0x0E => {
                        if (param >> 4) == 0x0E {
                            pattern_delay = (param & 0x0F) as u32;
                        }
                    }
                    _ => {}
                }
            }

            // Add row time (with pattern delay multiplier)
            total_ms += row_ms * (1 + pattern_delay) as f64;
            row += 1;
        }

        pos += 1;
    }

    // Return duration in seconds, minimum 1 second
    (total_ms / 1000.0).max(1.0) as u32
}
