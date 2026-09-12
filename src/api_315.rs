//! REST calls added in Ultimate firmware 3.15.
//!
//! Kept apart from [`crate::api`] so the version gate is impossible to miss:
//! everything here 404s on older firmware. Callers check
//! [`crate::device_caps::DeviceCaps::has_315_api`] first; the calls themselves
//! also classify a 404/501 answer rather than reporting a generic HTTP error,
//! because on this API those two codes carry distinct, actionable meanings:
//!
//! * **404** — the firmware predates the call.
//! * **501** — the firmware has it, but this hardware cannot do it. A cartridge
//!   answers 501 to `machine:input`, since driving keyboard and joystick lines
//!   needs Ultimate 64-class hardware.
//!
//! Verified live against an Ultimate II+L on firmware 3.15.

use crate::device_caps::CapError;
use crate::net_utils::{
    build_device_client, build_device_client_ms, with_password, REST_TIMEOUT_SECS,
};
use serde::{Deserialize, Serialize};

/// Formatting a disk image writes the whole thing, so give it room — a DNP can
/// be many megabytes. The observed D64 format took well under a second, but the
/// budget has to cover the largest case on the slowest medium.
const CREATE_TIMEOUT_SECS: u64 = 60;

/// Percent-encode a device path while leaving the `/` separators intact.
///
/// The spec asks for the path to be URL encoded, but it is carried as a *path*
/// segment between the route and the `:command`, so the slashes have to survive
/// or the device sees a single flat name.
fn encode_path(path: &str) -> String {
    path.trim_start_matches('/')
        .split('/')
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The device's own description of a failure, or the bare status when it sent
/// none.
fn device_message(status: u16, body: &str) -> String {
    serde_json::from_str::<ErrorResponse>(body)
        .ok()
        .and_then(|e| e.errors.into_iter().next())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("HTTP {}", status))
}

/// Map a response status onto the meanings this API gives it.
///
/// Only 404 and 501 carry capability meaning. Everything else is the device
/// refusing one specific request — reporting that as a version problem would
/// tell the user to upgrade firmware over something like `FILE EXISTS`.
fn classify(status: u16, body: &str) -> CapError {
    let msg = device_message(status, body);
    match status {
        501 => CapError::Unsupported(msg),
        404 => CapError::TooOld { found: msg },
        _ => CapError::Device(msg),
    }
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    #[serde(default)]
    errors: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────
//  Disk image creation  (PUT /v1/files/{path}:create_*)
// ─────────────────────────────────────────────────────────────────

/// A disk image the firmware can format in place.
///
/// Before 3.15 the app built these in memory and uploaded them over FTP; the
/// firmware now formats them on the device from a single call. `Dnp` has no
/// local equivalent at all — it is native-partition format, sized in 64 KB
/// tracks, and only reachable through this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskKind {
    /// 1541. `tracks` defaults to 35; 40 is the common extended format.
    D64 { tracks: Option<u32> },
    /// 1571.
    D71,
    /// 1581.
    D81,
    /// Native partition, `tracks` × 64 KB. Required, not optional.
    Dnp { tracks: u32 },
}

impl DiskKind {
    /// The `:command` suffix for this kind.
    fn command(&self) -> &'static str {
        match self {
            DiskKind::D64 { .. } => "create_d64",
            DiskKind::D71 => "create_d71",
            DiskKind::D81 => "create_d81",
            DiskKind::Dnp { .. } => "create_dnp",
        }
    }

    /// Conventional file extension.
    pub fn extension(&self) -> &'static str {
        match self {
            DiskKind::D64 { .. } => "d64",
            DiskKind::D71 => "d71",
            DiskKind::D81 => "d81",
            DiskKind::Dnp { .. } => "dnp",
        }
    }

    /// Query arguments beyond `diskname`.
    fn extra_args(&self) -> Vec<(&'static str, String)> {
        match self {
            DiskKind::D64 { tracks: Some(t) } => vec![("tracks", t.to_string())],
            DiskKind::Dnp { tracks } => vec![("tracks", tracks.to_string())],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateResponse {
    #[serde(default)]
    bytes_written: u64,
}

/// Format an empty disk image on the device. Returns the byte count written.
///
/// `path` is the destination on the device, e.g. `/Usb0/games/new.d64`.
pub async fn create_disk_image(
    host: &str,
    password: Option<&str>,
    path: &str,
    kind: DiskKind,
    diskname: Option<&str>,
) -> Result<u64, String> {
    let url = format!(
        "http://{}/v1/files/{}:{}",
        host,
        encode_path(path),
        kind.command()
    );
    let mut args = kind.extra_args();
    if let Some(name) = diskname.filter(|n| !n.is_empty()) {
        args.push(("diskname", name.to_string()));
    }

    let client = build_device_client(CREATE_TIMEOUT_SECS)?;
    let req = with_password(client.put(&url).query(&args), password);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(classify(status, &body).to_string());
    }
    let parsed: CreateResponse =
        serde_json::from_str(&body).unwrap_or(CreateResponse { bytes_written: 0 });
    Ok(parsed.bytes_written)
}

// ─────────────────────────────────────────────────────────────────
//  File info  (GET /v1/files/{path}:info)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct FileInfo {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub extension: String,
}

#[derive(Debug, Deserialize)]
struct FileInfoResponse {
    files: Option<FileInfo>,
}

/// Read one file's metadata. `Ok(None)` when the file does not exist (404),
/// which is a normal answer here rather than a failure.
pub async fn file_info(
    host: &str,
    password: Option<&str>,
    path: &str,
) -> Result<Option<FileInfo>, String> {
    let url = format!("http://{}/v1/files/{}:info", host, encode_path(path));
    let client = build_device_client(REST_TIMEOUT_SECS)?;
    let req = with_password(client.get(&url), password);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    match status {
        404 => Ok(None),
        s if (200..300).contains(&s) => Ok(serde_json::from_str::<FileInfoResponse>(&body)
            .map_err(|e| e.to_string())?
            .files),
        s => Err(classify(s, &body).to_string()),
    }
}

// ─────────────────────────────────────────────────────────────────
//  Heap statistics  (GET /v1/machine:heap)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct HeapStats {
    #[serde(default)]
    pub free: u64,
    #[serde(default)]
    pub min_ever_free: u64,
    #[serde(default)]
    pub total: u64,
}

impl HeapStats {
    /// Bytes currently in use.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// Fraction of the heap in use, 0.0–1.0. Zero when the total is unknown.
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f32 / self.total as f32
        }
    }
}

/// Read the firmware's FreeRTOS heap counters.
pub async fn heap_stats(host: &str, password: Option<&str>) -> Result<HeapStats, String> {
    let url = format!("http://{}/v1/machine:heap", host);
    let client = build_device_client(REST_TIMEOUT_SECS)?;
    let req = with_password(client.get(&url), password);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(classify(status, &body).to_string());
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

// ─────────────────────────────────────────────────────────────────
//  Menu screen  (GET /v1/machine:menu_screen)
// ─────────────────────────────────────────────────────────────────

/// Width of the Ultimate menu screen, in characters.
pub const MENU_COLS: usize = 40;
/// Height of the Ultimate menu screen, in characters.
pub const MENU_ROWS: usize = 25;
/// Cells in one menu screen.
pub const MENU_CELLS: usize = MENU_COLS * MENU_ROWS;

/// A snapshot of the screen the Ultimate menu is drawing — not the screen of
/// the running C64 program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuScreen {
    /// 1000 screen codes, in reading order.
    pub codes: Vec<u8>,
    /// 1000 colour values for the same cells.
    pub colors: Vec<u8>,
}

impl MenuScreen {
    /// Split the firmware's 2000-byte attachment into codes and colours.
    pub fn from_bytes(raw: &[u8]) -> Result<Self, String> {
        if raw.len() != MENU_CELLS * 2 {
            return Err(format!(
                "expected {} bytes of menu screen, got {}",
                MENU_CELLS * 2,
                raw.len()
            ));
        }
        let (codes, colors) = raw.split_at(MENU_CELLS);
        Ok(Self {
            codes: codes.to_vec(),
            colors: colors.to_vec(),
        })
    }

    /// One row of screen codes.
    pub fn row(&self, row: usize) -> &[u8] {
        let start = row * MENU_COLS;
        &self.codes[start..start + MENU_COLS]
    }
}

/// Read the menu screen. `Ok(None)` when the menu is not on screen — the
/// firmware answers 404, which the spec calls the cheapest way to ask whether
/// the menu is open.
pub async fn menu_screen(host: &str, password: Option<&str>) -> Result<Option<MenuScreen>, String> {
    let url = format!("http://{}/v1/machine:menu_screen", host);
    let client = build_device_client(REST_TIMEOUT_SECS)?;
    let req = with_password(client.get(&url), password);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(classify(status, &body).to_string());
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    MenuScreen::from_bytes(&bytes).map(Some)
}

// ─────────────────────────────────────────────────────────────────
//  Keyboard / joystick injection  (POST /v1/machine:input)
// ─────────────────────────────────────────────────────────────────

/// What an input event does to the inputs it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    /// Hold until something releases it.
    Press,
    /// Let it go.
    Release,
    /// Press and release in one call.
    Tap,
}

/// A direction or button of a C64 joystick. `fire2`/`fire3` need a pad that
/// has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JoyInput {
    Up,
    Down,
    Left,
    Right,
    Fire,
    Fire2,
    Fire3,
}

/// One event in a batch. The firmware validates the whole batch before
/// applying any of it, so a rejected batch changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum InputEvent {
    Keyboard {
        inputs: Vec<String>,
        transition: Transition,
    },
    Joystick {
        port: u8,
        inputs: Vec<JoyInput>,
        transition: Transition,
    },
    /// Drop everything currently held. Worth sending when a mapping session
    /// ends, so a held direction doesn't stick on the device.
    ReleaseAll,
}

#[derive(Debug, Clone, Serialize)]
struct InputBatch<'a> {
    events: &'a [InputEvent],
}

/// Budget for one input batch.
///
/// Deliberately short. This is interactive control: an event that has not
/// landed in this long is already stale, and the caller re-sends the current
/// stick position rather than replaying the old one. A long timeout here froze
/// the joystick for seconds whenever the device was briefly busy.
pub const INPUT_TIMEOUT_MS: u64 = 500;

/// Largest batch the firmware accepts.
pub const MAX_INPUT_EVENTS: usize = 64;

/// Apply keyboard/joystick events.
///
/// Returns [`CapError::Unsupported`] when the device has the call but not the
/// hardware — every cartridge answers 501 here, so callers must treat that as
/// a routine outcome and fall back to their existing method rather than
/// surfacing it as a failure.
pub async fn send_input(
    host: &str,
    password: Option<&str>,
    events: &[InputEvent],
) -> Result<(), CapError> {
    if events.is_empty() {
        return Ok(());
    }
    if events.len() > MAX_INPUT_EVENTS {
        return Err(CapError::Unsupported(format!(
            "batch of {} events exceeds the firmware limit of {}",
            events.len(),
            MAX_INPUT_EVENTS
        )));
    }
    let url = format!("http://{}/v1/machine:input", host);
    let client = build_device_client_ms(INPUT_TIMEOUT_MS).map_err(CapError::Unsupported)?;
    let req = with_password(client.post(&url), password).json(&InputBatch { events });
    let resp = req
        .send()
        .await
        .map_err(|e| CapError::Unsupported(e.to_string()))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(classify(status, &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_path_keeps_separators_and_escapes_segments() {
        assert_eq!(encode_path("/Usb0/games/new.d64"), "Usb0/games/new.d64");
        assert_eq!(encode_path("Temp/my disk.d64"), "Temp/my%20disk.d64");
        // A leading slash is dropped so the URL doesn't gain an empty segment.
        assert_eq!(encode_path("/Temp/x.d64"), "Temp/x.d64");
    }

    #[test]
    fn disk_kinds_map_to_their_commands_and_extensions() {
        assert_eq!(DiskKind::D64 { tracks: None }.command(), "create_d64");
        assert_eq!(DiskKind::D71.command(), "create_d71");
        assert_eq!(DiskKind::D81.command(), "create_d81");
        assert_eq!(DiskKind::Dnp { tracks: 8 }.command(), "create_dnp");
        assert_eq!(DiskKind::D81.extension(), "d81");
    }

    /// `tracks` is optional for a D64 but required for a DNP, which is sized
    /// in 64 KB tracks and has no sensible default.
    #[test]
    fn only_the_kinds_that_take_tracks_send_it() {
        assert!(DiskKind::D64 { tracks: None }.extra_args().is_empty());
        assert!(DiskKind::D71.extra_args().is_empty());
        assert!(DiskKind::D81.extra_args().is_empty());
        assert_eq!(
            DiskKind::D64 { tracks: Some(40) }.extra_args(),
            vec![("tracks", "40".to_string())]
        );
        assert_eq!(
            DiskKind::Dnp { tracks: 16 }.extra_args(),
            vec![("tracks", "16".to_string())]
        );
    }

    #[test]
    fn menu_screen_splits_codes_from_colours() {
        let mut raw = vec![0u8; MENU_CELLS * 2];
        raw[0] = 0x03; // 'C'
        raw[MENU_CELLS] = 0x0e; // colour of that cell
        let s = MenuScreen::from_bytes(&raw).expect("2000 bytes is the valid size");
        assert_eq!(s.codes.len(), MENU_CELLS);
        assert_eq!(s.colors.len(), MENU_CELLS);
        assert_eq!(s.codes[0], 0x03);
        assert_eq!(s.colors[0], 0x0e);
        assert_eq!(s.row(0).len(), MENU_COLS);
    }

    #[test]
    fn menu_screen_rejects_a_short_attachment() {
        assert!(MenuScreen::from_bytes(&[0u8; 100]).is_err());
    }

    #[test]
    fn heap_percentages_survive_a_zero_total() {
        let h = HeapStats {
            free: 5_329_584,
            min_ever_free: 5_305_888,
            total: 8_388_608,
        };
        assert_eq!(h.used(), 8_388_608 - 5_329_584);
        assert!((h.used_fraction() - 0.365).abs() < 0.01);
        let empty = HeapStats {
            free: 0,
            min_ever_free: 0,
            total: 0,
        };
        assert_eq!(empty.used_fraction(), 0.0, "must not divide by zero");
    }

    /// The wire shape is dictated by the firmware schema, so pin it.
    #[test]
    fn joystick_event_serialises_to_the_documented_shape() {
        let ev = InputEvent::Joystick {
            port: 2,
            inputs: vec![JoyInput::Up, JoyInput::Fire],
            transition: Transition::Press,
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(j["kind"], "joystick");
        assert_eq!(j["port"], 2);
        assert_eq!(j["transition"], "press");
        assert_eq!(j["inputs"][0], "up");
        assert_eq!(j["inputs"][1], "fire");
    }

    #[test]
    fn keyboard_and_release_all_serialise_to_the_documented_shape() {
        let k = InputEvent::Keyboard {
            inputs: vec!["return".into()],
            transition: Transition::Tap,
        };
        let j: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&k).unwrap()).unwrap();
        assert_eq!(j["kind"], "keyboard");
        assert_eq!(j["transition"], "tap");
        assert_eq!(j["inputs"][0], "return");

        let r: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&InputEvent::ReleaseAll).unwrap()).unwrap();
        assert_eq!(r["kind"], "releaseall");
    }

    /// 501 means "this hardware can't", which callers fall back on; anything
    /// else is treated as the firmware being too old.
    #[test]
    fn status_501_classifies_as_unsupported_not_too_old() {
        let body =
            r#"{"errors":["Keyboard and joystick injection require Ultimate 64-class hardware."]}"#;
        match classify(501, body) {
            CapError::Unsupported(m) => assert!(m.contains("Ultimate 64-class"), "{m}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(matches!(
            classify(404, r#"{"errors":["not found"]}"#),
            CapError::TooOld { .. }
        ));
    }

    /// A device refusing one request must not be reported as a firmware
    /// version problem — that told users to upgrade over `FILE EXISTS`.
    #[test]
    fn an_operational_refusal_is_not_reported_as_a_version_problem() {
        let err = classify(400, r#"{"errors":["FILE EXISTS"]}"#);
        assert!(
            matches!(err, CapError::Device(ref m) if m == "FILE EXISTS"),
            "got {err:?}"
        );
        let shown = err.to_string();
        assert_eq!(shown, "FILE EXISTS");
        assert!(
            !shown.contains("3.15"),
            "must not blame the firmware version: {shown}"
        );
    }

    #[test]
    fn a_body_without_an_error_falls_back_to_the_status() {
        assert_eq!(device_message(500, ""), "HTTP 500");
        assert_eq!(device_message(500, r#"{"errors":[]}"#), "HTTP 500");
    }
}
