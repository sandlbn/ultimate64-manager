//! Commodore 64 BASIC v2 source → PRG tokenizer.
//!
//! Pure logic, no UI. Walks plain UTF-8 source line by line, matches keywords
//! against the BASIC v2 token table, translates string contents to PETSCII
//! (with petcat-style `{CLR}` / `{$93}` control codes), and emits the standard
//! PRG layout — `$01 $08` load address, linked lines, `$00 $00` terminator.
//!
//! Reference: <https://www.c64-wiki.com/wiki/BASIC_token>.

use std::collections::HashMap;

/// Standard load address for a BASIC program on the unexpanded C64.
pub const LOAD_ADDRESS: u16 = 0x0801;

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramError {
    /// 1-indexed source line in the editor.
    pub line: usize,
    /// 1-indexed column inside that source line.
    pub col: usize,
    /// BASIC line number when the source line had a parseable one.
    pub line_number: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line_number {
            Some(n) => write!(
                f,
                "Line {} (BASIC {}): col {}: {}",
                self.line, n, self.col, self.message
            ),
            None => write!(f, "Line {}: col {}: {}", self.line, self.col, self.message),
        }
    }
}

// -----------------------------------------------------------------------------
// BASIC v2 keyword table
// -----------------------------------------------------------------------------

/// 76 BASIC v2 keywords + π. Token byte values as documented in the C64 ROM.
///
/// **Order matters**: callers iterate this table to find longest matches, so
/// the table is sorted descending by source length at module init. That keeps
/// `GOSUB` from being mistaken for `GO` followed by `SUB`.
const RAW_KEYWORDS: &[(&str, u8)] = &[
    ("END", 0x80),
    ("FOR", 0x81),
    ("NEXT", 0x82),
    ("DATA", 0x83),
    ("INPUT#", 0x84),
    ("INPUT", 0x85),
    ("DIM", 0x86),
    ("READ", 0x87),
    ("LET", 0x88),
    ("GOTO", 0x89),
    ("RUN", 0x8A),
    ("IF", 0x8B),
    ("RESTORE", 0x8C),
    ("GOSUB", 0x8D),
    ("RETURN", 0x8E),
    ("REM", 0x8F),
    ("STOP", 0x90),
    ("ON", 0x91),
    ("WAIT", 0x92),
    ("LOAD", 0x93),
    ("SAVE", 0x94),
    ("VERIFY", 0x95),
    ("DEF", 0x96),
    ("POKE", 0x97),
    ("PRINT#", 0x98),
    ("PRINT", 0x99),
    ("CONT", 0x9A),
    ("LIST", 0x9B),
    ("CLR", 0x9C),
    ("CMD", 0x9D),
    ("SYS", 0x9E),
    ("OPEN", 0x9F),
    ("CLOSE", 0xA0),
    ("GET", 0xA1),
    ("NEW", 0xA2),
    ("TAB(", 0xA3),
    ("TO", 0xA4),
    ("FN", 0xA5),
    ("SPC(", 0xA6),
    ("THEN", 0xA7),
    ("NOT", 0xA8),
    ("STEP", 0xA9),
    ("+", 0xAA),
    ("-", 0xAB),
    ("*", 0xAC),
    ("/", 0xAD),
    ("^", 0xAE),
    ("AND", 0xAF),
    ("OR", 0xB0),
    (">", 0xB1),
    ("=", 0xB2),
    ("<", 0xB3),
    ("SGN", 0xB4),
    ("INT", 0xB5),
    ("ABS", 0xB6),
    ("USR", 0xB7),
    ("FRE", 0xB8),
    ("POS", 0xB9),
    ("SQR", 0xBA),
    ("RND", 0xBB),
    ("LOG", 0xBC),
    ("EXP", 0xBD),
    ("COS", 0xBE),
    ("SIN", 0xBF),
    ("TAN", 0xC0),
    ("ATN", 0xC1),
    ("PEEK", 0xC2),
    ("LEN", 0xC3),
    ("STR$", 0xC4),
    ("VAL", 0xC5),
    ("ASC", 0xC6),
    ("CHR$", 0xC7),
    ("LEFT$", 0xC8),
    ("RIGHT$", 0xC9),
    ("MID$", 0xCA),
    ("GO", 0xCB),
];

/// Lazy-cached keyword table sorted by source length (descending). Built once
/// per process — keyword count is small enough that the cost is irrelevant.
fn keywords_sorted() -> &'static Vec<(&'static str, u8)> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<(&'static str, u8)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut v: Vec<(&'static str, u8)> = RAW_KEYWORDS.to_vec();
        v.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        v
    })
}

// -----------------------------------------------------------------------------
// PETSCII control-code names (petcat convention)
// -----------------------------------------------------------------------------

/// `{NAME}` → PETSCII byte. Names are matched case-insensitively. Spaces
/// between words are allowed (`RVS ON` and `RVSON` both match).
const RAW_CONTROL: &[(&str, u8)] = &[
    // Cursor + screen control
    ("WHITE", 0x05),
    ("WHT", 0x05),
    ("DISABLE SHIFT", 0x08),
    ("CURSOR DISABLE", 0x08),
    ("ENABLE SHIFT", 0x09),
    ("CURSOR ENABLE", 0x09),
    ("RETURN", 0x0D),
    ("RET", 0x0D),
    ("LOWERCASE", 0x0E),
    ("LOWER CASE", 0x0E),
    ("RVS ON", 0x12),
    ("REVERSE ON", 0x12),
    ("HOME", 0x13),
    ("DELETE", 0x14),
    ("DEL", 0x14),
    ("RED", 0x1C),
    ("CRSR RIGHT", 0x1D),
    ("RIGHT", 0x1D),
    ("GREEN", 0x1E),
    ("GRN", 0x1E),
    ("BLUE", 0x1F),
    ("BLU", 0x1F),
    // F-keys
    ("F1", 0x85),
    ("F3", 0x86),
    ("F5", 0x87),
    ("F7", 0x88),
    ("F2", 0x89),
    ("F4", 0x8A),
    ("F6", 0x8B),
    ("F8", 0x8C),
    ("SHIFT RETURN", 0x8D),
    ("UPPERCASE", 0x8E),
    ("UPPER CASE", 0x8E),
    ("BLACK", 0x90),
    ("BLK", 0x90),
    ("CRSR UP", 0x91),
    ("UP", 0x91),
    ("RVS OFF", 0x92),
    ("REVERSE OFF", 0x92),
    ("CLR", 0x93),
    ("CLEAR", 0x93),
    ("INSERT", 0x94),
    ("INST", 0x94),
    ("BROWN", 0x95),
    ("LIGHT RED", 0x96),
    ("LRED", 0x96),
    ("PINK", 0x96),
    ("DARK GREY", 0x97),
    ("DARK GRAY", 0x97),
    ("GREY 1", 0x97),
    ("GREY", 0x98),
    ("GRAY", 0x98),
    ("GREY 2", 0x98),
    ("MID GREY", 0x98),
    ("LIGHT GREEN", 0x99),
    ("LGRN", 0x99),
    ("LIGHT BLUE", 0x9A),
    ("LBLU", 0x9A),
    ("LIGHT GREY", 0x9B),
    ("LIGHT GRAY", 0x9B),
    ("GREY 3", 0x9B),
    ("PURPLE", 0x9C),
    ("PUR", 0x9C),
    ("CRSR LEFT", 0x9D),
    ("LEFT", 0x9D),
    ("YELLOW", 0x9E),
    ("YEL", 0x9E),
    ("CYAN", 0x9F),
    ("CYN", 0x9F),
    ("CRSR DOWN", 0x11),
    ("DOWN", 0x11),
    ("ORANGE", 0x81),
    ("ORNG", 0x81),
    ("PI", 0xFF),
];

fn control_table() -> &'static HashMap<String, u8> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<HashMap<String, u8>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for (name, byte) in RAW_CONTROL {
            m.insert(normalize_control_name(name), *byte);
        }
        m
    })
}

/// Strip whitespace + uppercase so `{rvs on}` and `{RVSON}` both look up.
fn normalize_control_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

// -----------------------------------------------------------------------------
// Tokenization
// -----------------------------------------------------------------------------

/// Tokenize a complete BASIC source program into a runnable PRG.
///
/// Returns the raw bytes including the `$01 $08` load address header and the
/// trailing `$00 $00` end-of-program marker. On failure, returns every error
/// found so the user can fix the program in one pass.
pub fn tokenize_program(source: &str) -> Result<Vec<u8>, Vec<ProgramError>> {
    let mut errors: Vec<ProgramError> = Vec::new();
    let mut tokenized_lines: Vec<(u16, Vec<u8>)> = Vec::new();
    let mut last_line_no: Option<u16> = None;

    for (idx, raw_line) in source.lines().enumerate() {
        let source_line_no = idx + 1;
        if raw_line.trim().is_empty() {
            continue;
        }
        match tokenize_line(raw_line, source_line_no) {
            Ok((line_no, payload)) => {
                if let Some(prev) = last_line_no {
                    if line_no <= prev {
                        errors.push(ProgramError {
                            line: source_line_no,
                            col: 1,
                            line_number: Some(line_no),
                            message: format!(
                                "BASIC line numbers must ascend ({} follows {})",
                                line_no, prev
                            ),
                        });
                        continue;
                    }
                }
                last_line_no = Some(line_no);
                tokenized_lines.push((line_no, payload));
            }
            Err(mut errs) => errors.append(&mut errs),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // Assemble the PRG. Each in-memory line:
    //   <next-line-addr lo> <next-line-addr hi> <line-no lo> <line-no hi>
    //   <tokenized payload bytes> <0x00>
    // Then a final 0x00 0x00 terminator.
    let mut out = Vec::with_capacity(
        2 + tokenized_lines
            .iter()
            .map(|(_, p)| p.len() + 5)
            .sum::<usize>()
            + 2,
    );
    out.extend_from_slice(&LOAD_ADDRESS.to_le_bytes());

    // Compute each line's start address so we can write its predecessor's
    // next-line pointer correctly. Memory offset 0 corresponds to LOAD_ADDRESS.
    let mut cursor: u16 = LOAD_ADDRESS;
    let starts: Vec<u16> = tokenized_lines
        .iter()
        .map(|(_, payload)| {
            let here = cursor;
            // line layout = 2 (next-ptr) + 2 (line-no) + payload + 1 (terminator)
            cursor = cursor.wrapping_add(5 + payload.len() as u16);
            here
        })
        .collect();

    for (i, (line_no, payload)) in tokenized_lines.iter().enumerate() {
        // Next-line pointer: the start of the line after this one. For the
        // last line, this points at where the trailing 0x00 0x00 sits — the
        // C64 BASIC interpreter reads that as "next line address = 0" and
        // stops, which is the standard convention.
        let next_ptr = match starts.get(i + 1) {
            Some(addr) => *addr,
            None => cursor, // address of the upcoming end-of-program marker
        };
        out.extend_from_slice(&next_ptr.to_le_bytes());
        out.extend_from_slice(&line_no.to_le_bytes());
        out.extend_from_slice(payload);
        out.push(0x00);
    }
    // End-of-program marker.
    out.push(0x00);
    out.push(0x00);

    Ok(out)
}

/// Same checks as [`tokenize_program`] but returns just the parsed BASIC line
/// numbers on success — used by the editor's Validate button when we don't
/// need the PRG bytes.
pub fn validate(source: &str) -> Result<Vec<u16>, Vec<ProgramError>> {
    let mut errors: Vec<ProgramError> = Vec::new();
    let mut nums: Vec<u16> = Vec::new();
    let mut last: Option<u16> = None;
    for (idx, raw_line) in source.lines().enumerate() {
        let source_line_no = idx + 1;
        if raw_line.trim().is_empty() {
            continue;
        }
        match tokenize_line(raw_line, source_line_no) {
            Ok((n, _)) => {
                if let Some(prev) = last {
                    if n <= prev {
                        errors.push(ProgramError {
                            line: source_line_no,
                            col: 1,
                            line_number: Some(n),
                            message: format!(
                                "BASIC line numbers must ascend ({} follows {})",
                                n, prev
                            ),
                        });
                        continue;
                    }
                }
                last = Some(n);
                nums.push(n);
            }
            Err(mut e) => errors.append(&mut e),
        }
    }
    if errors.is_empty() {
        Ok(nums)
    } else {
        Err(errors)
    }
}

/// Tokenize one source line. Returns `(BASIC line number, payload bytes)` —
/// the payload excludes the next-line pointer and BASIC line number bytes,
/// and it does NOT include the trailing $00 (the assembler appends that).
pub fn tokenize_line(
    raw: &str,
    source_line_no: usize,
) -> Result<(u16, Vec<u8>), Vec<ProgramError>> {
    let mut errors: Vec<ProgramError> = Vec::new();

    // ── parse the leading BASIC line number ──
    let bytes = raw.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && bytes[pos] == b' ' {
        pos += 1;
    }
    let num_start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        pos += 1;
    }
    if pos == num_start {
        errors.push(ProgramError {
            line: source_line_no,
            col: pos + 1,
            line_number: None,
            message: "expected BASIC line number".into(),
        });
        return Err(errors);
    }
    let line_no: u32 = raw[num_start..pos].parse().unwrap_or(u32::MAX);
    if line_no > 63999 {
        errors.push(ProgramError {
            line: source_line_no,
            col: num_start + 1,
            line_number: None,
            message: format!("BASIC line number {} exceeds 63999", line_no),
        });
        return Err(errors);
    }
    let line_no = line_no as u16;
    while pos < bytes.len() && bytes[pos] == b' ' {
        pos += 1;
    }

    // ── walk the body ──
    let body = &raw[pos..];
    let body_offset = pos; // for column reporting
    let mut payload: Vec<u8> = Vec::with_capacity(body.len() + 4);
    let mut in_string = false;
    // After REM ($8F) we copy raw PETSCII to end-of-line.
    let mut after_rem = false;
    let kw = keywords_sorted();
    let mut i = 0usize;
    let body_bytes = body.as_bytes();

    while i < body_bytes.len() {
        let c = body_bytes[i];
        let col = body_offset + i + 1;

        if after_rem {
            payload.push(ascii_to_petscii(c));
            i += 1;
            continue;
        }

        if in_string {
            if c == b'"' {
                payload.push(b'"');
                in_string = false;
                i += 1;
                continue;
            }
            if c == b'{' {
                // Find matching '}'. PETSCII control codes never contain it.
                let close = body_bytes[i + 1..].iter().position(|&b| b == b'}');
                let close = match close {
                    Some(off) => i + 1 + off,
                    None => {
                        errors.push(ProgramError {
                            line: source_line_no,
                            col,
                            line_number: Some(line_no),
                            message: "unterminated PETSCII control code (missing `}`)".into(),
                        });
                        return Err(errors);
                    }
                };
                let inside = &body[i + 1..close];
                match parse_control_code(inside) {
                    Some(byte) => payload.push(byte),
                    None => {
                        errors.push(ProgramError {
                            line: source_line_no,
                            col,
                            line_number: Some(line_no),
                            message: format!("unknown PETSCII control code: {{{}}}", inside),
                        });
                        return Err(errors);
                    }
                }
                i = close + 1;
                continue;
            }
            payload.push(ascii_to_petscii(c));
            i += 1;
            continue;
        }

        // Outside a string.
        if c == b'"' {
            payload.push(b'"');
            in_string = true;
            i += 1;
            continue;
        }
        if c == b' ' {
            payload.push(0x20);
            i += 1;
            continue;
        }

        // Try the longest keyword that matches at this position. Comparison
        // is case-insensitive — the user types `print` or `PRINT` and we
        // match either, then emit the canonical token byte.
        let rest_upper: Vec<u8> = body_bytes[i..]
            .iter()
            .take(8) // longest keyword is 6 (`RETURN`/`RIGHT$`); 8 is generous
            .map(|b| b.to_ascii_uppercase())
            .collect();
        let mut matched: Option<(&str, u8)> = None;
        for &(name, token) in kw.iter() {
            if rest_upper.starts_with(name.as_bytes()) {
                matched = Some((name, token));
                break;
            }
        }
        if let Some((name, token)) = matched {
            payload.push(token);
            i += name.len();
            if token == 0x8F {
                after_rem = true;
            }
            continue;
        }

        // No keyword — emit one PETSCII byte.
        payload.push(ascii_to_petscii(c));
        i += 1;
    }

    if in_string {
        errors.push(ProgramError {
            line: source_line_no,
            col: body_offset + body_bytes.len() + 1,
            line_number: Some(line_no),
            message: "unterminated string (missing closing `\"`)".into(),
        });
    }

    if errors.is_empty() {
        Ok((line_no, payload))
    } else {
        Err(errors)
    }
}

// -----------------------------------------------------------------------------
// Char conversions
// -----------------------------------------------------------------------------

/// Map an ASCII byte to its PETSCII equivalent for upper/graphics mode (the
/// C64's default). Lowercase a-z fold to uppercase A-Z so what the user types
/// matches what the C64 displays without further configuration.
fn ascii_to_petscii(c: u8) -> u8 {
    match c {
        b'a'..=b'z' => c - 0x20, // fold to uppercase 0x41..0x5A
        // 0x00..0x1F control bytes other than tab/newline shouldn't appear in
        // editor input, but if they do, pass through — useful for any future
        // direct PETSCII paste.
        _ => c,
    }
}

/// Parse a `{...}` control-code body. Supports:
/// - Hex byte: `{$93}` or `{$NN}` → that byte literally
/// - Decimal: `{147}` → that byte (lets users paste from CHR$ tables)
/// - Named: `{CLR}`, `{RVS ON}`, `{rvs on}` (case + space insensitive)
fn parse_control_code(body: &str) -> Option<u8> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed.strip_prefix('$') {
        return u8::from_str_radix(hex.trim(), 16).ok();
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return trimmed.parse().ok();
    }
    let key = normalize_control_name(trimmed);
    control_table().get(&key).copied()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(src: &str) -> Vec<u8> {
        tokenize_program(src).unwrap_or_else(|e| panic!("tokenize failed: {:?}", e))
    }

    #[test]
    fn empty_program_still_has_load_address() {
        let bytes = tok("");
        assert_eq!(&bytes[..2], &[0x01, 0x08], "load address must be $0801 LE");
        assert_eq!(
            &bytes[2..],
            &[0x00, 0x00],
            "empty program is just the end marker"
        );
    }

    #[test]
    fn print_hello_tokenizes_correctly() {
        let bytes = tok("10 PRINT \"HELLO\"\n");
        // $01 $08 | next-ptr (LE) | line-no 10 (LE) | $99 PRINT | $20 sp |
        // $22 " | H E L L O | $22 " | $00 EOL | $00 $00 EOP
        assert_eq!(&bytes[..2], &[0x01, 0x08]);
        // Line number bytes at offsets 4-5 are 0x0A 0x00 (= 10).
        assert_eq!(bytes[4], 10);
        assert_eq!(bytes[5], 0);
        // PRINT token must appear.
        assert!(
            bytes.contains(&0x99),
            "PRINT token (0x99) missing: {:?}",
            bytes
        );
        // Trailing end-of-program marker.
        assert_eq!(&bytes[bytes.len() - 2..], &[0x00, 0x00]);
    }

    #[test]
    fn rem_body_is_not_tokenized() {
        let bytes = tok("10 REM PRINT \"X\"\n");
        // $8F (REM) appears once. $99 (PRINT) must NOT appear — it's part of
        // the REM body and stays as plain PETSCII letters.
        assert_eq!(bytes.iter().filter(|&&b| b == 0x8F).count(), 1);
        assert!(
            !bytes.contains(&0x99),
            "PRINT got tokenized inside REM: {:?}",
            bytes
        );
    }

    #[test]
    fn petscii_clr_in_string() {
        let bytes = tok("10 PRINT \"{CLR}HI\"\n");
        // After the opening quote we expect $93 (CLR).
        let q = bytes
            .iter()
            .position(|&b| b == 0x22)
            .expect("opening quote");
        assert_eq!(
            bytes[q + 1],
            0x93,
            "CLR byte missing after quote: {:?}",
            &bytes[q..]
        );
    }

    #[test]
    fn petscii_hex_form() {
        let bytes = tok("10 PRINT \"{$0d}\"\n");
        let q = bytes.iter().position(|&b| b == 0x22).unwrap();
        assert_eq!(bytes[q + 1], 0x0D);
    }

    #[test]
    fn petscii_decimal_form() {
        let bytes = tok("10 PRINT \"{147}\"\n");
        let q = bytes.iter().position(|&b| b == 0x22).unwrap();
        assert_eq!(bytes[q + 1], 147);
    }

    #[test]
    fn unknown_control_code_is_an_error() {
        let err = tokenize_program("10 PRINT \"{NOPE}\"\n").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("NOPE"));
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let err = tokenize_program("10 PRINT \"oh no\n").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unterminated string"));
    }

    #[test]
    fn unterminated_control_code_is_an_error() {
        let err = tokenize_program("10 PRINT \"{CLR\n").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unterminated PETSCII"));
    }

    #[test]
    fn descending_line_numbers_rejected() {
        let err = tokenize_program("20 PRINT 1\n10 PRINT 2\n").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("ascend"));
    }

    #[test]
    fn line_numbers_above_max_rejected() {
        let err = tokenize_program("64000 PRINT 1\n").unwrap_err();
        assert!(err[0].message.contains("63999"));
    }

    #[test]
    fn missing_line_number_rejected() {
        let err = tokenize_program("PRINT 1\n").unwrap_err();
        assert!(err[0].message.contains("expected BASIC line number"));
    }

    #[test]
    fn lowercase_input_folds_to_uppercase() {
        // "print" tokenizes the same as "PRINT" — case-insensitive keywords.
        let lower = tok("10 print 1\n");
        let upper = tok("10 PRINT 1\n");
        assert_eq!(lower, upper);
    }

    #[test]
    fn longest_match_wins_gosub_vs_go() {
        let bytes = tok("10 GOSUB 100\n");
        // $8D is GOSUB. If the table mis-sorted, we'd see $CB (GO) + plain "SUB".
        assert!(bytes.contains(&0x8D));
        assert!(!bytes.contains(&0xCB));
    }

    #[test]
    fn arithmetic_operators_tokenize() {
        let bytes = tok("10 LET A=1+2*3\n");
        assert!(bytes.contains(&0xB2)); // =
        assert!(bytes.contains(&0xAA)); // +
        assert!(bytes.contains(&0xAC)); // *
    }

    #[test]
    fn multi_line_link_pointers_make_sense() {
        let bytes = tok("10 PRINT 1\n20 PRINT 2\n");
        // First line starts at file offset 2 (memory $0801).
        // Read its next-line pointer.
        let next_ptr = u16::from_le_bytes([bytes[2], bytes[3]]);
        // Line 1 length = 2 (next-ptr) + 2 (line-no) + payload + 1 (EOL).
        // Payload: $99 PRINT, $20 space, $31 '1' = 3 bytes. So line len = 8.
        // Next-ptr should be $0801 + 8 = $0809.
        assert_eq!(next_ptr, 0x0809);
        // The byte at file offset (next_ptr - LOAD_ADDRESS + 2) should be the
        // start of line 2's next pointer (some address), then line-no 20.
        let line2_offset = (next_ptr - LOAD_ADDRESS) as usize + 2;
        assert_eq!(
            u16::from_le_bytes([bytes[line2_offset + 2], bytes[line2_offset + 3]]),
            20,
            "line 2's BASIC line number should be 20"
        );
    }

    #[test]
    fn validate_returns_line_numbers_in_order() {
        let nums = validate("10 PRINT 1\n20 PRINT 2\n100 PRINT 3\n").unwrap();
        assert_eq!(nums, vec![10, 20, 100]);
    }

    #[test]
    fn blank_lines_skipped() {
        let bytes = tok("\n\n10 PRINT 1\n\n20 PRINT 2\n\n");
        // Both lines tokenize fine despite the blank padding.
        assert_eq!(bytes.iter().filter(|&&b| b == 0x99).count(), 2);
    }

    #[test]
    fn errors_collected_across_multiple_lines() {
        let err = tokenize_program("10 PRINT \"a\n20 PRINT \"b\n").unwrap_err();
        assert_eq!(err.len(), 2, "both lines should be reported: {:?}", err);
    }

    #[test]
    fn control_code_names_are_case_and_space_insensitive() {
        let a = tok("10 PRINT \"{rvs on}\"\n");
        let b = tok("10 PRINT \"{RVSON}\"\n");
        let c = tok("10 PRINT \"{Reverse On}\"\n");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }
}
