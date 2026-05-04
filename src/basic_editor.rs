//! Iced UI for the BASIC editor tab.
//!
//! Layout:
//! - Toolbar: New / Open .bas / Save .prg / Validate / Send & Run + status pill
//! - Multi-line `text_editor` with hand-rolled BASIC v2 highlighter
//! - Status bar with the last validation outcome
//!
//! All BASIC parsing logic lives in [`crate::basic_tokenizer`]; this module is
//! just plumbing and presentation.

use iced::advanced::text::highlighter::Format;
use iced::advanced::text::Highlighter as HighlighterTrait;
use iced::widget::{
    button, column, container, row, rule, text,
    text_editor::{Action, Content},
    tooltip, Space,
};
use iced::{Color, Element, Font, Length, Task};
use std::ops::Range;
use std::path::PathBuf;

use crate::basic_tokenizer::{self, ProgramError};

const SEND_TIMEOUT_SECS: u64 = 30;

/// One-shot starter program so the empty editor isn't intimidating.
const STARTER_PROGRAM: &str = "10 PRINT \"{CLR}HELLO ULTIMATE64\"\n20 GOTO 10\n";

#[derive(Debug, Clone)]
pub enum BasicEditorMessage {
    Edit(Action),
    Validate,
    SendAndRun,
    SendCompleted(Result<String, String>),
    NewProgram,
    OpenFile,
    OpenCompleted(Result<(PathBuf, String), String>),
    SavePrg,
    SavePrgCompleted(Result<PathBuf, String>),
}

pub struct BasicEditor {
    content: Content,
    last_validation: Option<Result<Vec<u16>, Vec<ProgramError>>>,
    is_sending: bool,
    status_message: Option<String>,
    current_file: Option<PathBuf>,
}

impl Default for BasicEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl BasicEditor {
    pub fn new() -> Self {
        Self {
            content: Content::with_text(STARTER_PROGRAM),
            last_validation: None,
            is_sending: false,
            status_message: None,
            current_file: None,
        }
    }

    fn source(&self) -> String {
        self.content.text()
    }

    pub fn update(
        &mut self,
        message: BasicEditorMessage,
        host: Option<String>,
        password: Option<String>,
    ) -> Task<BasicEditorMessage> {
        use BasicEditorMessage as M;
        match message {
            M::Edit(action) => {
                // Any edit invalidates the last validation result — the user
                // shouldn't see "✓ OK" against text they've since changed.
                if matches!(
                    action,
                    Action::Edit(_) | Action::Drag(_) | Action::SelectAll
                ) {
                    self.last_validation = None;
                }
                self.content.perform(action);
                Task::none()
            }
            M::Validate => {
                let src = self.source();
                let result = basic_tokenizer::validate(&src);
                self.status_message = Some(format_validation(&result));
                self.last_validation = Some(result);
                Task::none()
            }
            M::SendAndRun => {
                let host = match host.filter(|h| !h.is_empty()) {
                    Some(h) => h,
                    None => {
                        self.status_message = Some("Set the device host in Settings first".into());
                        return Task::none();
                    }
                };
                let src = self.source();
                let bytes = match basic_tokenizer::tokenize_program(&src) {
                    Ok(b) => b,
                    Err(errs) => {
                        let result: Result<Vec<u16>, Vec<ProgramError>> = Err(errs);
                        self.status_message = Some(format_validation(&result));
                        self.last_validation = Some(result);
                        return Task::none();
                    }
                };
                self.is_sending = true;
                self.status_message = Some(format!("Sending {} bytes to device…", bytes.len()));
                Task::perform(
                    async move {
                        let host_url = if host.starts_with("http") {
                            host
                        } else {
                            format!("http://{}", host)
                        };
                        tokio::time::timeout(
                            std::time::Duration::from_secs(SEND_TIMEOUT_SECS),
                            crate::api::upload_runner_async(
                                &host_url,
                                "run_prg",
                                bytes,
                                password.as_deref(),
                            ),
                        )
                        .await
                        .map_err(|_| "Send timed out".to_string())?
                        .map(|_| "Running on device".to_string())
                    },
                    M::SendCompleted,
                )
            }
            M::SendCompleted(result) => {
                self.is_sending = false;
                self.status_message = Some(match result {
                    Ok(s) => s,
                    Err(e) => format!("Send failed: {}", e),
                });
                Task::none()
            }
            M::NewProgram => {
                self.content = Content::with_text("10 \n");
                self.current_file = None;
                self.last_validation = None;
                self.status_message = Some("New program".into());
                Task::none()
            }
            M::OpenFile => Task::perform(
                async move {
                    let handle = rfd::AsyncFileDialog::new()
                        .add_filter("BASIC source", &["bas", "txt"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .ok_or_else(|| "Cancelled".to_string())?;
                    let path = handle.path().to_path_buf();
                    let bytes = handle.read().await;
                    let text = String::from_utf8(bytes)
                        .map_err(|e| format!("File is not UTF-8 text: {}", e))?;
                    Ok((path, text))
                },
                M::OpenCompleted,
            ),
            M::OpenCompleted(result) => {
                match result {
                    Ok((path, text)) => {
                        self.content = Content::with_text(&text);
                        self.current_file = Some(path.clone());
                        self.last_validation = None;
                        self.status_message = Some(format!("Opened {}", path.display()));
                    }
                    Err(e) if e == "Cancelled" => {}
                    Err(e) => {
                        self.status_message = Some(format!("Open failed: {}", e));
                    }
                }
                Task::none()
            }
            M::SavePrg => {
                let src = self.source();
                let bytes = match basic_tokenizer::tokenize_program(&src) {
                    Ok(b) => b,
                    Err(errs) => {
                        let result: Result<Vec<u16>, Vec<ProgramError>> = Err(errs);
                        self.status_message =
                            Some(format!("Cannot save — {}", format_validation(&result)));
                        self.last_validation = Some(result);
                        return Task::none();
                    }
                };
                let suggested_name = self
                    .current_file
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .and_then(|s| s.to_str())
                    .map(|s| format!("{}.prg", s))
                    .unwrap_or_else(|| "program.prg".to_string());
                Task::perform(
                    async move {
                        let handle = rfd::AsyncFileDialog::new()
                            .add_filter("PRG", &["prg"])
                            .set_file_name(&suggested_name)
                            .save_file()
                            .await
                            .ok_or_else(|| "Cancelled".to_string())?;
                        let path = handle.path().to_path_buf();
                        tokio::fs::write(&path, &bytes)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(path)
                    },
                    M::SavePrgCompleted,
                )
            }
            M::SavePrgCompleted(result) => {
                match result {
                    Ok(path) => {
                        self.status_message = Some(format!("Saved {}", path.display()));
                    }
                    Err(e) if e == "Cancelled" => {}
                    Err(e) => {
                        self.status_message = Some(format!("Save failed: {}", e));
                    }
                }
                Task::none()
            }
        }
    }

    pub fn view(&self, font_size: u32, is_connected: bool) -> Element<'_, BasicEditorMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let toolbar = row![
            tool_button(
                "New",
                "Start a fresh BASIC program",
                BasicEditorMessage::NewProgram,
                fs.normal
            ),
            tool_button(
                "Open .bas",
                "Load a BASIC text file",
                BasicEditorMessage::OpenFile,
                fs.normal
            ),
            tool_button(
                "Save .prg",
                "Tokenize and save as PRG",
                BasicEditorMessage::SavePrg,
                fs.normal
            ),
            Space::new().width(15),
            tool_button(
                "Validate",
                "Check the program for errors without sending",
                BasicEditorMessage::Validate,
                fs.normal
            ),
            send_button(self.is_sending, BasicEditorMessage::SendAndRun, fs.normal),
            Space::new().width(Length::Fill),
            connection_pill(is_connected, fs.small),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);

        let editor = text_editor(&self.content)
            .placeholder("10 PRINT \"HELLO\"")
            .on_action(BasicEditorMessage::Edit)
            .font(Font::MONOSPACE)
            .size(fs.normal)
            .height(Length::Fill)
            .padding(8)
            .highlight_with::<BasicHighlighter>((), highlight_format);

        let status_text = self.status_message.clone().unwrap_or_else(|| {
            if self.is_sending {
                "Sending…".to_string()
            } else {
                "Ready — click Validate or Send & Run".to_string()
            }
        });
        let file_label: String = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_name().and_then(|s| s.to_str()).map(String::from))
            .unwrap_or_else(|| "untitled".to_string());

        let status = container(
            row![
                text(status_text).size(fs.small).color(self.status_color()),
                Space::new().width(Length::Fill),
                text(file_label)
                    .size(fs.tiny)
                    .color(Color::from_rgb(0.55, 0.55, 0.6)),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([4, 8]);

        // Don't wrap `text_editor` in `scrollable` — the widget already
        // owns its own scroll viewport. Double-wrapping was triggering
        // `iced_tiny_skia` to panic in `Build rounded rectangle path` on
        // Linux when the inner rounded background got sized below 2× its
        // corner radius. Letting text_editor fill the column directly
        // sidesteps the issue.
        column![
            toolbar,
            rule::horizontal(1),
            editor,
            rule::horizontal(1),
            status,
        ]
        .spacing(5)
        .padding(5)
        .into()
    }

    fn status_color(&self) -> Color {
        match &self.last_validation {
            Some(Ok(_)) => Color::from_rgb(0.45, 0.85, 0.55),
            Some(Err(_)) => Color::from_rgb(0.9, 0.55, 0.45),
            None => Color::from_rgb(0.7, 0.7, 0.75),
        }
    }
}

// -----------------------------------------------------------------------------
// Toolbar widgets
// -----------------------------------------------------------------------------

fn tool_button<'a>(
    label: &'a str,
    hint: &'a str,
    on_press: BasicEditorMessage,
    size: u32,
) -> Element<'a, BasicEditorMessage> {
    tooltip(
        button(text(label).size(size))
            .on_press(on_press)
            .padding([6, 12]),
        hint,
        tooltip::Position::Bottom,
    )
    .style(container::bordered_box)
    .into()
}

fn send_button<'a>(
    is_sending: bool,
    on_press: BasicEditorMessage,
    size: u32,
) -> Element<'a, BasicEditorMessage> {
    let label = if is_sending {
        "Sending…"
    } else {
        "▶ Send & Run"
    };
    let mut btn = button(text(label).size(size)).padding([6, 14]);
    if !is_sending {
        btn = btn.on_press(on_press);
    }
    tooltip(
        btn,
        "Tokenize the program and run it on the Ultimate64",
        tooltip::Position::Bottom,
    )
    .style(container::bordered_box)
    .into()
}

fn connection_pill<'a>(is_connected: bool, size: u32) -> Element<'a, BasicEditorMessage> {
    if is_connected {
        text("● Connected")
            .size(size)
            .color(Color::from_rgb(0.2, 0.8, 0.2))
            .into()
    } else {
        text("○ Not connected")
            .size(size)
            .color(Color::from_rgb(0.8, 0.5, 0.2))
            .into()
    }
}

// -----------------------------------------------------------------------------
// Validation result formatting
// -----------------------------------------------------------------------------

fn format_validation(result: &Result<Vec<u16>, Vec<ProgramError>>) -> String {
    match result {
        Ok(nums) if nums.is_empty() => "Empty program".into(),
        Ok(nums) => format!(
            "✓ {} line(s), BASIC {}–{} OK",
            nums.len(),
            nums.first().unwrap(),
            nums.last().unwrap()
        ),
        Err(errs) if errs.len() == 1 => format!("✗ {}", errs[0]),
        Err(errs) => format!("✗ {} errors — first: {}", errs.len(), errs[0]),
    }
}

// -----------------------------------------------------------------------------
// Syntax highlighter
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum Highlight {
    LineNumber,
    Keyword,
    String,
    ControlCode,
    RemBody,
    Number,
}

/// Per-line BASIC highlighter. Each `highlight_line` call is independent —
/// REM and string state never cross newlines in BASIC v2, so there's no
/// inter-line state to track.
pub struct BasicHighlighter {
    current: usize,
}

impl HighlighterTrait for BasicHighlighter {
    type Settings = ();
    type Highlight = Highlight;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Highlight)>;

    fn new(_settings: &Self::Settings) -> Self {
        Self { current: 0 }
    }

    fn update(&mut self, _new_settings: &Self::Settings) {}

    fn change_line(&mut self, line: usize) {
        self.current = line;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let spans = highlight_basic_line(line);
        self.current = self.current.saturating_add(1);
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

fn highlight_format(h: &Highlight, _theme: &iced::Theme) -> Format<Font> {
    let color = match h {
        Highlight::LineNumber => Color::from_rgb(0.55, 0.7, 0.95),
        Highlight::Keyword => Color::from_rgb(0.85, 0.7, 0.2),
        Highlight::String => Color::from_rgb(0.5, 0.85, 0.55),
        Highlight::ControlCode => Color::from_rgb(0.85, 0.55, 0.85),
        Highlight::RemBody => Color::from_rgb(0.55, 0.55, 0.6),
        Highlight::Number => Color::from_rgb(0.75, 0.85, 0.95),
    };
    Format {
        color: Some(color),
        font: None,
    }
}

/// Walk one source line and emit highlight spans. Mirrors the tokenizer's
/// state machine but doesn't fail on errors — invalid input just gets no
/// special highlight, so the user can still see what they're typing.
fn highlight_basic_line(line: &str) -> Vec<(Range<usize>, Highlight)> {
    let mut spans: Vec<(Range<usize>, Highlight)> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let num_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > num_start {
        spans.push((num_start..i, Highlight::LineNumber));
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }

    let kw_table = keywords_descending();
    let mut in_string = false;
    let mut after_rem = false;

    while i < bytes.len() {
        if after_rem {
            spans.push((i..bytes.len(), Highlight::RemBody));
            break;
        }
        let c = bytes[i];
        if in_string {
            if c == b'"' {
                spans.push((i..i + 1, Highlight::String));
                in_string = false;
                i += 1;
                continue;
            }
            if c == b'{' {
                if let Some(off) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                    let end = i + 1 + off + 1;
                    spans.push((i..end, Highlight::ControlCode));
                    i = end;
                    continue;
                }
                // Unclosed `{` while typing — treat the rest as plain string.
                spans.push((i..bytes.len(), Highlight::String));
                break;
            }
            spans.push((i..i + 1, Highlight::String));
            i += 1;
            continue;
        }

        if c == b'"' {
            spans.push((i..i + 1, Highlight::String));
            in_string = true;
            i += 1;
            continue;
        }

        let upper: Vec<u8> = bytes[i..]
            .iter()
            .take(8)
            .map(|b| b.to_ascii_uppercase())
            .collect();
        let mut matched: Option<(usize, u8)> = None;
        for (name, token) in kw_table.iter() {
            if upper.starts_with(name.as_bytes()) {
                matched = Some((name.len(), *token));
                break;
            }
        }
        if let Some((len, token)) = matched {
            spans.push((i..i + len, Highlight::Keyword));
            i += len;
            if token == 0x8F {
                after_rem = true;
            }
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            spans.push((start..i, Highlight::Number));
            continue;
        }

        i += 1;
    }
    spans
}

/// Local mirror of the tokenizer keyword table — sorted descending by length
/// for greedy matching. Re-derived rather than re-exported to keep
/// `basic_tokenizer` private to its tokenization API.
fn keywords_descending() -> &'static Vec<(String, u8)> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<(String, u8)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        const RAW: &[(&str, u8)] = &[
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
            ("AND", 0xAF),
            ("OR", 0xB0),
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
        let mut v: Vec<(String, u8)> = RAW.iter().map(|(s, b)| (s.to_string(), *b)).collect();
        v.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        v
    })
}

// Local re-export so `text_editor(...)` resolves to the iced widget builder.
fn text_editor<'a>(
    content: &'a Content,
) -> iced::widget::TextEditor<'a, iced::advanced::text::highlighter::PlainText, BasicEditorMessage>
{
    iced::widget::text_editor(content)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn span_kinds(line: &str) -> Vec<Highlight> {
        highlight_basic_line(line)
            .into_iter()
            .map(|(_, k)| k)
            .collect()
    }

    #[test]
    fn line_number_gets_its_own_span() {
        let kinds = span_kinds("10 PRINT \"HI\"");
        assert!(matches!(kinds.first(), Some(Highlight::LineNumber)));
        assert!(kinds.iter().any(|k| matches!(k, Highlight::Keyword)));
        assert!(kinds.iter().any(|k| matches!(k, Highlight::String)));
    }

    #[test]
    fn rem_body_is_one_long_span() {
        let spans = highlight_basic_line("10 REM PRINT GOTO");
        let kinds: Vec<_> = spans.iter().map(|(_, k)| k).collect();
        let keyword_count = kinds
            .iter()
            .filter(|k| matches!(k, Highlight::Keyword))
            .count();
        let rem_count = kinds
            .iter()
            .filter(|k| matches!(k, Highlight::RemBody))
            .count();
        assert_eq!(keyword_count, 1, "only REM should highlight as keyword");
        assert_eq!(rem_count, 1, "REM body collapses to a single span");
    }

    #[test]
    fn control_code_is_a_distinct_span() {
        let kinds = span_kinds("10 PRINT \"{CLR}HI\"");
        assert!(
            kinds.iter().any(|k| matches!(k, Highlight::ControlCode)),
            "expected a ControlCode span, got: {:?}",
            kinds
        );
    }

    #[test]
    fn unterminated_string_does_not_panic() {
        let _ = highlight_basic_line("10 PRINT \"oh no");
        let _ = highlight_basic_line("10 PRINT \"{CLR");
    }

    #[test]
    fn empty_line_yields_no_spans() {
        assert!(highlight_basic_line("").is_empty());
        assert!(highlight_basic_line("   ").is_empty());
    }

    #[test]
    fn validation_format_summarizes_success() {
        let s = format_validation(&Ok(vec![10, 20, 100]));
        assert!(s.contains("3 line"), "{}", s);
        assert!(s.contains("10") && s.contains("100"), "{}", s);
    }

    #[test]
    fn validation_format_summarizes_failure() {
        let err = vec![ProgramError {
            line: 2,
            col: 5,
            line_number: Some(20),
            message: "unterminated string".into(),
        }];
        let s = format_validation(&Err(err));
        assert!(s.starts_with('✗'));
        assert!(s.contains("unterminated"));
    }

    #[test]
    fn editor_starts_with_runnable_program() {
        let editor = BasicEditor::new();
        let result = basic_tokenizer::validate(&editor.source());
        assert!(result.is_ok(), "starter must validate: {:?}", result);
    }
}
