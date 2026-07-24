//! Game Mode — an immersive, EmulationStation/Kodi-style launcher over the
//! game folders configured in Settings. Self-contained: it owns its state,
//! messages, library scanning, cover/screenshot loading, and the launcher
//! view. The host [`RemoteBrowser`] embeds a [`GameMode`], forwards
//! [`GameModeMessage`]s to [`GameMode::update`], and turns a returned
//! [`GameUpdate::launch`] into its existing run/mount actions.
//!
//! ## Library layouts
//!
//! Real C64 collections are laid out several ways. Rather than one blind
//! heuristic, the scanner **detects** the layout per root and picks a strategy
//! (see [`LibraryLayout`] / [`detect_layout`]): flat files at the root
//! (OneLoad64), files bucketed by letter, one folder per game, or arbitrary
//! nesting. Art is resolved from images sitting next to a game *or* from a
//! central art folder (`Extras/Images/Screenshots`, `LoadingScreens`, …)
//! matched by the game's basename.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::{button, column, container, row, rule, scrollable, stack, text, Column, Space};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Subscription, Task, Theme};
use serde::{Deserialize, Serialize};

use crate::ftp_ops::{download_file_ftp_preview, fetch_files_ftp, RemoteFileEntry};

/// Stable widget id for the game list scrollable — used to programmatically
/// scroll the highlighted game into view.
const GAME_LIST_SCROLLABLE_ID: &str = "game_mode_list";

/// Approximate rendered height of one list row (game or section header), in
/// logical pixels — used to compute scroll offsets. Matches the row padding +
/// font in [`GameMode::view`].
const ROW_HEIGHT_PX: f32 = 30.0;
/// Bias the scroll target up by a slice of the viewport so the selection isn't
/// pinned to the bottom edge after a Down press.
const VIEWPORT_GUESS_PX: f32 = 420.0;

// ── Scan tuning / extension points ───────────────────────────────────────────
const MAX_DEPTH: u32 = 4;
// Generous runaway backstops — a folder-per-game library visits one dir per
// game, so these must comfortably exceed real collection sizes (OneLoad64 is
// ~2145). They only guard against pathological/looping trees.
const MAX_GAMES: usize = 20000;
const MAX_DIRS: usize = 40000;
/// How many of a root's subdirs to sample when detecting the layout.
const DETECT_SAMPLE: usize = 8;
/// Candidate central art folders probed under each root (relative paths).
/// Order does not matter; matching is by the game's basename.
const ART_FOLDER_CANDIDATES: [&str; 8] = [
    "Extras/Images/LoadingScreens",
    "Extras/Images/Screenshots",
    "LoadingScreens",
    "Screenshots",
    "Images",
    "Snaps",
    "Covers",
    "Media",
];

// ─────────────────────────────────────────────────────────────────────────────
//  Messages
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum GameModeMessage {
    /// Enter (with the library roots from Settings) or leave the launcher.
    Toggle(Vec<String>),
    /// Force a re-scan, bypassing the cache (user pressed Refresh).
    Refresh,
    /// Library scan finished.
    Enumerated(Result<ScanResult, String>),
    /// Highlight a game by index (click).
    Select(usize),
    /// Move the highlight by a delta (keyboard ↑/↓).
    Nav(i32),
    /// Jump to the first game whose section letter matches.
    JumpToLetter(char),
    /// Box art + screenshot for a game finished downloading (best-effort).
    ArtLoaded(String, (Option<Vec<u8>>, Option<Vec<u8>>)),
    /// Launch the highlighted game.
    Run,
    /// Toggle immersive fullscreen (hide app chrome + OS fullscreen).
    ToggleFullscreen,
    /// The list scrolled — carries the current viewport height (for centering).
    ListScrolled(f32),
    /// Advance the animated background one frame.
    AnimTick,
}

/// FTP connection info handed to [`GameMode::update`] by the host each call, so
/// the module never needs to reach back into `RemoteBrowser`.
#[derive(Debug, Clone, Default)]
pub struct GameCtx {
    pub host: Option<String>,
    pub password: Option<String>,
}

/// A launch request handed back to the host: which file to run and whether it
/// lives on the local disk (upload to the device) or the device itself.
pub struct Launch {
    pub path: String,
    pub local: bool,
}

/// Result of [`GameMode::update`]. `task` is the module's own follow-up work;
/// `launch`, when set, is a game the host should run/mount.
pub struct GameUpdate {
    pub task: Task<GameModeMessage>,
    pub launch: Option<Launch>,
}

impl GameUpdate {
    fn task(task: Task<GameModeMessage>) -> Self {
        Self { task, launch: None }
    }
    fn none() -> Self {
        Self {
            task: Task::none(),
            launch: None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in the launcher. Run target and art image paths are resolved up
/// front during the scan so launching and art loading don't re-list the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEntry {
    pub title: String,
    /// Concrete file the launcher runs (`.prg`/`.crt`/disk image). `None` if the
    /// folder had images but nothing runnable.
    pub run_path: Option<String>,
    pub cover_path: Option<String>,
    pub shot_path: Option<String>,
    /// Stable identity for cached art / in-flight dedup.
    pub key: String,
    /// Section letter (`A`–`Z`, or `#` for digits/symbols) for the A–Z rail.
    pub letter: char,
    /// True when this game lives on the local filesystem (vs the device FTP).
    /// Determines how art is read and how the game is launched.
    pub local: bool,
}

/// Decoded box art + screenshot for a game.
#[derive(Debug, Clone, Default)]
pub struct GameArt {
    pub cover: Option<iced::widget::image::Handle>,
    pub shot: Option<iced::widget::image::Handle>,
}

/// Outcome of a library scan.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub games: Vec<GameEntry>,
    /// Per-root human-readable layout labels (for the header + logs).
    pub layout_label: String,
}

/// Per-folder classification primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderKind {
    /// Several runnable files — each is its own game.
    Flat,
    /// A single runnable — the whole folder is one game.
    GameFolder,
    /// No runnables — a grouping folder to recurse into.
    Group,
}

/// Detected library layout for a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibraryLayout {
    FlatFiles,
    FileBuckets,
    GameFolders,
    Grouped,
}

impl LibraryLayout {
    fn label(self) -> &'static str {
        match self {
            LibraryLayout::FlatFiles => "flat files",
            LibraryLayout::FileBuckets => "letter buckets",
            LibraryLayout::GameFolders => "game folders",
            LibraryLayout::Grouped => "grouped",
        }
    }
}

/// Central art index built once per root: basename → image path, for each kind
/// of art folder found.
#[derive(Debug, Default, Clone)]
struct ArtIndex {
    loadingscreens: HashMap<String, String>,
    screenshots: HashMap<String, String>,
    images: HashMap<String, String>,
}

impl ArtIndex {
    fn is_empty(&self) -> bool {
        self.loadingscreens.is_empty() && self.screenshots.is_empty() && self.images.is_empty()
    }
    /// (cover, shot) for a basename from central art alone.
    fn lookup(&self, base: &str) -> (Option<String>, Option<String>) {
        let cover = self
            .loadingscreens
            .get(base)
            .or_else(|| self.screenshots.get(base))
            .or_else(|| self.images.get(base))
            .cloned();
        let shot = self
            .screenshots
            .get(base)
            .or_else(|| self.images.get(base))
            .cloned();
        (cover, shot)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  State
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default, Debug, Clone)]
pub struct GameMode {
    /// When true the File Browser renders this launcher instead of the panes.
    pub active: bool,
    /// When true the launcher fills the window with no app chrome (tab bar /
    /// status bar hidden). The host also puts the OS window in fullscreen.
    pub fullscreen: bool,
    /// Library roots (device paths and/or local folders) from Settings, kept so
    /// Refresh can re-scan without re-plumbing them.
    roots: Vec<String>,
    games: Vec<GameEntry>,
    loading: bool,
    selected: usize,
    error: Option<String>,
    layout_label: String,
    /// Decoded art keyed by `GameEntry.key`.
    art: HashMap<String, GameArt>,
    /// Keys whose art is being fetched (dedup guard).
    art_loading: HashSet<String>,
    /// Phase for the phosphor-glow background.
    anim_phase: f32,
    /// Last observed height of the list viewport, used to center the selection.
    list_viewport_h: f32,
}

impl GameMode {
    pub fn new() -> Self {
        Self::default()
    }

    /// Leave the launcher (Esc / exit button path).
    pub fn exit(&mut self) {
        self.active = false;
        self.fullscreen = false;
    }

    pub fn subscription(&self) -> Subscription<GameModeMessage> {
        if self.active {
            // ~20fps is plenty for the ambient background and keeps the big
            // list's per-frame rebuild affordable.
            iced::time::every(Duration::from_millis(50)).map(|_| GameModeMessage::AnimTick)
        } else {
            Subscription::none()
        }
    }

    pub fn update(&mut self, message: GameModeMessage, ctx: GameCtx) -> GameUpdate {
        match message {
            GameModeMessage::Toggle(roots) => {
                if self.active {
                    self.active = false;
                    self.fullscreen = false;
                    return GameUpdate::none();
                }
                self.active = true;
                self.error = None;
                self.selected = 0;
                self.games.clear();
                self.layout_label.clear();
                self.roots = roots.clone();
                if roots.is_empty() {
                    self.error = Some(
                        "No game library set. Add a folder in Settings → Preferences → Game library."
                            .to_string(),
                    );
                    return GameUpdate::none();
                }
                self.loading = true;
                GameUpdate::task(Task::perform(
                    load_or_scan(ctx.host.clone(), roots, ctx.password.clone(), false),
                    GameModeMessage::Enumerated,
                ))
            }

            GameModeMessage::Refresh => {
                if self.loading || self.roots.is_empty() {
                    return GameUpdate::none();
                }
                self.loading = true;
                self.error = None;
                GameUpdate::task(Task::perform(
                    load_or_scan(
                        ctx.host.clone(),
                        self.roots.clone(),
                        ctx.password.clone(),
                        true,
                    ),
                    GameModeMessage::Enumerated,
                ))
            }

            GameModeMessage::Enumerated(result) => {
                self.loading = false;
                match result {
                    Ok(scan) => {
                        self.games = scan.games;
                        self.layout_label = scan.layout_label;
                        self.selected = 0;
                        if self.games.is_empty() {
                            self.error =
                                Some("No games found under the configured library.".to_string());
                            GameUpdate::none()
                        } else {
                            self.error = None;
                            GameUpdate::task(self.load_art_for_selected(&ctx))
                        }
                    }
                    Err(e) => {
                        self.error = Some(e);
                        GameUpdate::none()
                    }
                }
            }

            GameModeMessage::Select(idx) => {
                if idx < self.games.len() {
                    self.selected = idx;
                    return GameUpdate::task(Task::batch([
                        self.load_art_for_selected(&ctx),
                        self.scroll_to_selected(),
                    ]));
                }
                GameUpdate::none()
            }

            GameModeMessage::Nav(delta) => {
                if self.games.is_empty() {
                    return GameUpdate::none();
                }
                let n = self.games.len() as i32;
                let next = (self.selected as i32 + delta).rem_euclid(n) as usize;
                if next != self.selected {
                    self.selected = next;
                    return GameUpdate::task(Task::batch([
                        self.load_art_for_selected(&ctx),
                        self.scroll_to_selected(),
                    ]));
                }
                GameUpdate::none()
            }

            GameModeMessage::JumpToLetter(letter) => {
                if let Some(idx) = self.games.iter().position(|g| g.letter == letter) {
                    self.selected = idx;
                    return GameUpdate::task(Task::batch([
                        self.load_art_for_selected(&ctx),
                        self.scroll_to_selected(),
                    ]));
                }
                GameUpdate::none()
            }

            GameModeMessage::ArtLoaded(key, (cover, shot)) => {
                self.art_loading.remove(&key);
                self.art.insert(
                    key,
                    GameArt {
                        cover: cover.map(iced::widget::image::Handle::from_bytes),
                        shot: shot.map(iced::widget::image::Handle::from_bytes),
                    },
                );
                GameUpdate::none()
            }

            GameModeMessage::Run => {
                let launch = self.games.get(self.selected).and_then(|g| {
                    g.run_path.clone().map(|path| Launch {
                        path,
                        local: g.local,
                    })
                });
                GameUpdate {
                    task: Task::none(),
                    launch,
                }
            }

            GameModeMessage::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                // The host reads `self.fullscreen` after this returns to put the
                // OS window in/out of fullscreen.
                GameUpdate::none()
            }

            GameModeMessage::ListScrolled(viewport_h) => {
                self.list_viewport_h = viewport_h;
                GameUpdate::none()
            }

            GameModeMessage::AnimTick => {
                self.anim_phase = (self.anim_phase + 0.05) % (std::f32::consts::TAU * 1000.0);
                GameUpdate::none()
            }
        }
    }

    /// Fetch the highlighted game's already-resolved art unless cached / in
    /// flight. Lazy — only the selected game loads, so a huge library doesn't
    /// hammer the device FTP.
    fn load_art_for_selected(&mut self, ctx: &GameCtx) -> Task<GameModeMessage> {
        let Some(game) = self.games.get(self.selected) else {
            return Task::none();
        };
        let key = game.key.clone();
        if self.art.contains_key(&key) || self.art_loading.contains(&key) {
            return Task::none();
        }
        if game.cover_path.is_none() && game.shot_path.is_none() {
            self.art.insert(key, GameArt::default());
            return Task::none();
        }
        let cover = game.cover_path.clone();
        let shot = game.shot_path.clone();
        let local = game.local;
        // Local art needs no host; device art requires one.
        if !local && ctx.host.is_none() {
            return Task::none();
        }
        let host = ctx.host.clone().unwrap_or_default();
        let password = ctx.password.clone();
        self.art_loading.insert(key.clone());
        Task::perform(
            download_game_art(local, host, cover, shot, password),
            move |res| GameModeMessage::ArtLoaded(key.clone(), res),
        )
    }

    /// Scroll the list so the highlighted game is centered. Rows are a uniform
    /// fixed height (see the view), so `rows_above * ROW_HEIGHT_PX` is the exact
    /// top of the selection; we then bias up by half the viewport to center it.
    fn scroll_to_selected(&self) -> Task<GameModeMessage> {
        if self.games.is_empty() {
            return Task::none();
        }
        let headers_above = headers_before(&self.games, self.selected);
        let rows_above = self.selected + headers_above;
        let row_top = rows_above as f32 * ROW_HEIGHT_PX;
        // Center on the real viewport when known; fall back to a guess before
        // the first scroll event arrives.
        let half = if self.list_viewport_h > 1.0 {
            self.list_viewport_h / 2.0
        } else {
            VIEWPORT_GUESS_PX / 2.0
        };
        let y = (row_top + ROW_HEIGHT_PX / 2.0 - half).max(0.0);
        iced::widget::operation::scroll_to(
            iced::widget::Id::new(GAME_LIST_SCROLLABLE_ID),
            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y },
        )
    }

    // ── View ─────────────────────────────────────────────────────────────────

    pub fn view(&self, font_size: u32) -> Element<'_, GameModeMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let ink = Color::from_rgb(0.90, 0.91, 0.95);
        let dim = Color::from_rgb(0.55, 0.57, 0.64);
        let accent = Color::from_rgb(0.35, 0.72, 1.0);

        let exit_btn = || {
            button(text("✕ Exit (Esc)").size(fs.small))
                .on_press(GameModeMessage::Toggle(Vec::new()))
                .padding([5, 12])
                .style(crate::styles::nav_button)
        };
        let refresh_btn = || {
            button(text("↻ Refresh").size(fs.small))
                .on_press_maybe((!self.loading).then_some(GameModeMessage::Refresh))
                .padding([5, 12])
                .style(crate::styles::nav_button)
        };
        let fullscreen_btn = || {
            let label = if self.fullscreen {
                "🗗 Windowed"
            } else {
                "⛶ Fullscreen"
            };
            button(text(label).size(fs.small))
                .on_press(GameModeMessage::ToggleFullscreen)
                .padding([5, 12])
                .style(crate::styles::nav_button)
        };

        if self.loading {
            return game_backdrop(
                self.anim_phase,
                column![
                    row![
                        text("🎮 GAME MODE").size(fs.large).color(accent),
                        Space::new().width(Length::Fill),
                        exit_btn(),
                    ]
                    .align_y(iced::Alignment::Center),
                    Space::new().height(Length::Fill),
                    text("Loading library…").size(fs.normal).color(dim),
                    Space::new().height(Length::Fill),
                ]
                .spacing(10)
                .into(),
            );
        }

        if let Some(err) = &self.error {
            return game_backdrop(
                self.anim_phase,
                column![
                    row![
                        text("🎮 GAME MODE").size(fs.large).color(accent),
                        Space::new().width(Length::Fill),
                        exit_btn(),
                    ]
                    .align_y(iced::Alignment::Center),
                    Space::new().height(Length::Fill),
                    text(err.clone()).size(fs.normal).color(dim),
                    Space::new().height(Length::Fill),
                ]
                .spacing(10)
                .into(),
            );
        }

        let total = self.games.len();
        let selected = self.games.get(self.selected);
        let selected_key = selected.map(|g| g.key.clone());
        let art = selected_key.as_ref().and_then(|k| self.art.get(k));
        let art_loading = selected_key
            .as_ref()
            .map(|k| self.art_loading.contains(k))
            .unwrap_or(false);

        // ── Art panels ────────────────────────────────────────────────────
        let placeholder = |label: &str| -> Element<'_, GameModeMessage> {
            container(text(label.to_string()).size(fs.small).color(dim))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_t: &Theme| container::Style {
                    background: Some(Color::from_rgb(0.10, 0.10, 0.14).into()),
                    border: iced::border::rounded(6),
                    ..Default::default()
                })
                .into()
        };
        let cover_el: Element<'_, GameModeMessage> = match art.and_then(|a| a.cover.as_ref()) {
            Some(h) => iced::widget::image(h.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain)
                .into(),
            None if art_loading => placeholder("Loading art…"),
            None => placeholder("No box art"),
        };
        let box_art = container(cover_el)
            .width(Length::Fixed(320.0))
            .height(Length::Fill);

        let shot_el: Element<'_, GameModeMessage> = match art.and_then(|a| a.shot.as_ref()) {
            Some(h) => iced::widget::image(h.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain)
                .into(),
            None if art_loading => placeholder("…"),
            None => placeholder("No screenshot"),
        };
        let screenshot = container(shot_el)
            .width(Length::Fixed(320.0))
            .height(Length::Fill);

        // ── Title list with section headers ───────────────────────────────
        let mut list_items: Vec<Element<'_, GameModeMessage>> = Vec::new();
        let mut prev_letter: Option<char> = None;
        for (i, game) in self.games.iter().enumerate() {
            if prev_letter != Some(game.letter) {
                prev_letter = Some(game.letter);
                list_items.push(
                    container(text(game.letter.to_string()).size(fs.small).color(accent))
                        .padding([0, 10])
                        // Uniform row height so scroll offsets are exact.
                        .center_y(Length::Fixed(ROW_HEIGHT_PX))
                        .into(),
                );
            }
            let is_sel = i == self.selected;
            // Selected title is a distinct warm gold; others stay muted grey.
            let sel_gold = Color::from_rgb(1.0, 0.82, 0.28);
            let label =
                text(game.title.clone())
                    .size(fs.normal)
                    .color(if is_sel { sel_gold } else { dim });
            list_items.push(
                button(label)
                    .on_press(GameModeMessage::Select(i))
                    .width(Length::Fill)
                    .height(Length::Fixed(ROW_HEIGHT_PX))
                    .padding([4, 10])
                    .style(move |_t: &Theme, _s| {
                        if is_sel {
                            // Translucent accent fill + a brighter accent outline
                            // so the highlight reads over the animated backdrop
                            // without hiding it.
                            button::Style {
                                background: Some(
                                    Color {
                                        r: 0.35,
                                        g: 0.72,
                                        b: 1.0,
                                        a: 0.30,
                                    }
                                    .into(),
                                ),
                                text_color: sel_gold,
                                border: iced::Border {
                                    color: Color {
                                        r: 1.0,
                                        g: 0.82,
                                        b: 0.28,
                                        a: 0.85,
                                    },
                                    width: 1.0,
                                    radius: 4.0.into(),
                                },
                                ..Default::default()
                            }
                        } else {
                            button::Style {
                                background: None,
                                text_color: Color::from_rgb(0.7, 0.72, 0.78),
                                ..Default::default()
                            }
                        }
                    })
                    .into(),
            );
        }
        let title_list = scrollable(Column::with_children(list_items).spacing(0))
            .id(iced::widget::Id::new(GAME_LIST_SCROLLABLE_ID))
            .on_scroll(|vp| GameModeMessage::ListScrolled(vp.bounds().height))
            .height(Length::Fill)
            .width(Length::Fill);

        // ── A–Z jump rail ─────────────────────────────────────────────────
        // No scrolling: the letters are distributed evenly down the full height
        // so the whole index is always visible, regardless of font size or how
        // short the window is. Each letter's slot shares the height equally.
        let current_letter = selected.map(|g| g.letter);
        let mut rail_letters: Vec<char> = self.games.iter().map(|g| g.letter).collect();
        rail_letters.dedup();
        // Keep the rail font modest and independent of the (large) content font
        // so ~27 letters keep fitting; clamp to a readable range.
        let rail_font = (fs.tiny as f32 + 1.0).clamp(11.0, 14.0);
        let mut rail_col = Column::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center);
        for c in rail_letters {
            let is_cur = Some(c) == current_letter;
            let cell = container(text(c.to_string()).size(rail_font).color(if is_cur {
                Color::from_rgb(1.0, 0.82, 0.28)
            } else {
                Color::from_rgb(0.80, 0.82, 0.90)
            }))
            .width(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill) // fills its equal share of the column height
            .style(move |_t: &Theme| {
                if is_cur {
                    container::Style {
                        background: Some(
                            Color {
                                r: 1.0,
                                g: 0.82,
                                b: 0.28,
                                a: 0.18,
                            }
                            .into(),
                        ),
                        border: iced::border::rounded(3),
                        ..Default::default()
                    }
                } else {
                    container::Style::default()
                }
            });
            rail_col = rail_col
                .push(iced::widget::mouse_area(cell).on_press(GameModeMessage::JumpToLetter(c)));
        }
        // Fixed-width faint panel so the letters read clearly over the backdrop.
        let rail = container(rail_col)
            .width(Length::Fixed(26.0))
            .height(Length::Fill)
            .padding([2, 2])
            .style(|_t: &Theme| container::Style {
                background: Some(Color::from_rgb(0.08, 0.09, 0.13).into()),
                border: iced::border::rounded(5),
                ..Default::default()
            });

        let sel_title = selected
            .map(|g| g.title.clone())
            .unwrap_or_else(|| "—".to_string());
        let can_run = selected.map(|g| g.run_path.is_some()).unwrap_or(false);
        let run_btn = button(text("▶  Run  (Enter)").size(fs.normal))
            .on_press_maybe(can_run.then_some(GameModeMessage::Run))
            .padding([8, 20])
            .style(crate::styles::action_button);

        let center = column![
            text(sel_title).size(fs.large).color(ink),
            rule::horizontal(1),
            row![title_list, rail].spacing(6).height(Length::Fill),
            row![
                run_btn,
                Space::new().width(Length::Fill),
                text(format!("{}/{}", self.selected + 1, total))
                    .size(fs.small)
                    .color(dim),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(8)
        .width(Length::Fill)
        .height(Length::Fill);

        let subtitle = if self.layout_label.is_empty() {
            format!("{} games", total)
        } else {
            format!("{} games · {}", total, self.layout_label)
        };

        game_backdrop(
            self.anim_phase,
            column![
                row![
                    text("🎮 GAME MODE").size(fs.large).color(accent),
                    Space::new().width(12),
                    text(subtitle).size(fs.tiny).color(dim),
                    Space::new().width(Length::Fill),
                    text("↑/↓ select · A–Z jump · Enter run · Esc exit")
                        .size(fs.tiny)
                        .color(dim),
                    Space::new().width(12),
                    refresh_btn(),
                    fullscreen_btn(),
                    exit_btn(),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
                Space::new().height(6),
                row![box_art, center, screenshot]
                    .spacing(16)
                    .height(Length::Fill),
            ]
            .spacing(8)
            .into(),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Animated background
// ─────────────────────────────────────────────────────────────────────────────

/// Dark, full-bleed backdrop with an animated phosphor-glow canvas behind the
/// launcher content.
fn game_backdrop(
    phase: f32,
    content: Element<'_, GameModeMessage>,
) -> Element<'_, GameModeMessage> {
    let bg: Element<'_, GameModeMessage> = Canvas::new(GameBg { phase })
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    container(stack![bg, content])
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .style(|_t: &Theme| container::Style {
            background: Some(Color::from_rgb(0.04, 0.04, 0.07).into()),
            text_color: Some(Color::from_rgb(0.9, 0.9, 0.95)),
            ..Default::default()
        })
        .into()
}

/// A few drifting sine waves drawn with a 3-pass phosphor bloom. Purely
/// decorative and self-animating off `phase`.
struct GameBg {
    phase: f32,
}

impl canvas::Program<GameModeMessage> for GameBg {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if w < 2.0 || h < 2.0 {
            return vec![frame.into_geometry()];
        }
        let tau = std::f32::consts::TAU;
        let waves = [
            (
                Color::from_rgb(0.20, 0.85, 1.00),
                0.60_f32,
                1.7_f32,
                0.28_f32,
            ),
            (Color::from_rgb(0.35, 1.00, 0.55), 0.90, 2.3, 0.52),
            (Color::from_rgb(1.00, 0.72, 0.25), 0.42, 1.1, 0.74),
        ];
        for (color, speed, freq, y_frac) in waves {
            let mid = h * y_frac;
            let amp = h * 0.09;
            for pass in 0..3_u8 {
                let (lw, alpha) = match pass {
                    0 => (9.0_f32, 0.035_f32),
                    1 => (3.0, 0.10),
                    _ => (1.4, 0.32),
                };
                let path = Path::new(|b| {
                    let steps = 96;
                    for s in 0..=steps {
                        let t = s as f32 / steps as f32;
                        let x = t * w;
                        let y = mid
                            + (t * freq * tau + self.phase * speed).sin() * amp
                            + (t * freq * 2.3 * tau - self.phase * speed * 0.7).sin() * amp * 0.32;
                        if s == 0 {
                            b.move_to(Point::new(x, y));
                        } else {
                            b.line_to(Point::new(x, y));
                        }
                    }
                });
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Color { a: alpha, ..color })
                        .with_width(lw),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Pure helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Number of section-header rows that precede game index `sel` (one per
/// distinct leading letter in `games[0..=sel]`).
fn headers_before(games: &[GameEntry], sel: usize) -> usize {
    let mut count = 0usize;
    let mut prev: Option<char> = None;
    for g in games.iter().take(sel + 1) {
        if prev != Some(g.letter) {
            prev = Some(g.letter);
            count += 1;
        }
    }
    count
}

/// Section letter for a title: first alphanumeric char, upper-cased; digits and
/// anything else fold to `#`.
fn leading_letter(title: &str) -> char {
    for ch in title.chars() {
        if ch.is_ascii_alphabetic() {
            return ch.to_ascii_uppercase();
        }
        if ch.is_ascii_digit() {
            return '#';
        }
    }
    '#'
}

/// Lower-cased file stem (name without its final extension).
fn stem_lower(name: impl AsRef<str>) -> String {
    let name = name.as_ref();
    let stem = match name.rfind('.') {
        Some(idx) if idx > 0 => &name[..idx],
        _ => name,
    };
    stem.to_ascii_lowercase()
}

/// Lower-cased extension (no dot). `Game.PRG` → `prg`; no extension → "".
fn file_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(idx) if idx > 0 && idx + 1 < name.len() => name[idx + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Prettify a filename into a display title: drop the extension, turn `_`/`.`
/// separators into spaces.
fn nice_title(name: &str) -> String {
    let stem = match name.rfind('.') {
        Some(idx) if idx > 0 => &name[..idx],
        _ => name,
    };
    let cleaned = stem.replace(['_', '.'], " ");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        name.to_string()
    } else {
        cleaned.to_string()
    }
}

/// Last `/`-separated segment of a device path.
fn path_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(idx) => trimmed[idx + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Choose a folder's cover image, RetroArch-style: (a) stem-match to a selected
/// program, else (b) a conventionally-named cover, else (c) first image. `None`
/// if the folder holds no images.
fn pick_cover(files: &[RemoteFileEntry], selected_stem: Option<&str>) -> Option<String> {
    let images: Vec<&RemoteFileEntry> = files
        .iter()
        .filter(|f| !f.is_dir && crate::file_types::is_image_file(&f.name))
        .collect();
    if images.is_empty() {
        return None;
    }
    if let Some(sel) = selected_stem {
        if let Some(m) = images.iter().find(|f| stem_lower(&f.name) == sel) {
            return Some(m.path.clone());
        }
    }
    const CONVENTIONAL: [&str; 6] = ["cover", "box", "front", "screenshot", "screen", "title"];
    if let Some(m) = images.iter().find(|f| {
        let stem = stem_lower(&f.name);
        CONVENTIONAL
            .iter()
            .any(|c| stem == *c || stem.starts_with(c))
    }) {
        return Some(m.path.clone());
    }
    Some(images[0].path.clone())
}

/// Pick a screenshot image distinct from the cover: prefer gameplay-shot names,
/// else the first image that isn't the cover. `None` if no second image.
fn pick_screenshot(files: &[RemoteFileEntry], cover_path: Option<&str>) -> Option<String> {
    let images: Vec<&RemoteFileEntry> = files
        .iter()
        .filter(|f| !f.is_dir && crate::file_types::is_image_file(&f.name))
        .filter(|f| Some(f.path.as_str()) != cover_path)
        .collect();
    if images.is_empty() {
        return None;
    }
    const HINTS: [&str; 5] = ["screen", "shot", "ingame", "gameplay", "action"];
    if let Some(m) = images.iter().find(|f| {
        let n = f.name.to_ascii_lowercase();
        HINTS.iter().any(|h| n.contains(h))
    }) {
        return Some(m.path.clone());
    }
    Some(images[0].path.clone())
}

/// The file a game folder should launch: prefer `.prg`, then `.crt`, then a
/// disk image. `None` if nothing runnable is present.
fn primary_runnable(files: &[RemoteFileEntry]) -> Option<&RemoteFileEntry> {
    let is = |f: &RemoteFileEntry, e: &str| !f.is_dir && file_ext(&f.name) == e;
    files
        .iter()
        .find(|f| is(f, "prg"))
        .or_else(|| files.iter().find(|f| is(f, "crt")))
        .or_else(|| {
            files
                .iter()
                .find(|f| !f.is_dir && crate::file_types::is_disk_image(&file_ext(&f.name)))
        })
}

/// Classify a folder from its runnable-file and subdir counts.
fn classify_folder(n_runnables: usize, _n_subdirs: usize) -> FolderKind {
    if n_runnables >= 2 {
        FolderKind::Flat
    } else if n_runnables == 1 {
        FolderKind::GameFolder
    } else {
        // No runnables — a grouping folder to recurse into (empty folders
        // simply yield nothing).
        FolderKind::Group
    }
}

fn runnable_files(files: &[RemoteFileEntry]) -> Vec<&RemoteFileEntry> {
    files
        .iter()
        .filter(|f| !f.is_dir && crate::file_types::is_runnable(&file_ext(&f.name)))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
//  Source abstraction — a library root is either a local folder or a device
//  FTP path. `local` is decided per root by whether the path exists on disk.
// ─────────────────────────────────────────────────────────────────────────────

/// True when `root` is an existing local directory (vs a device FTP path).
fn root_is_local(root: &str) -> bool {
    std::path::Path::new(root).is_dir()
}

/// List a directory from whichever source backs it.
async fn list_dir(
    local: bool,
    host: &str,
    path: &str,
    password: &Option<String>,
) -> Result<Vec<RemoteFileEntry>, String> {
    if local {
        list_local_dir(path)
    } else {
        fetch_files_ftp(host.to_string(), path.to_string(), password.clone()).await
    }
}

/// Read a local directory into the shared [`RemoteFileEntry`] shape.
fn list_local_dir(path: &str) -> Result<Vec<RemoteFileEntry>, String> {
    let read = std::fs::read_dir(path).map_err(|e| format!("{}: {}", path, e))?;
    let mut out = Vec::new();
    for entry in read.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(RemoteFileEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: meta.is_dir(),
            size: meta.len(),
            path: entry.path().to_string_lossy().to_string(),
        });
    }
    Ok(out)
}

/// Read an image's bytes from whichever source backs it.
async fn read_image(
    local: bool,
    host: &str,
    path: &str,
    password: &Option<String>,
) -> Option<Vec<u8>> {
    if local {
        std::fs::read(path).ok()
    } else {
        download_file_ftp_preview(host.to_string(), path.to_string(), password.clone())
            .await
            .ok()
            .map(|(_, b)| b)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Cache — persist the resolved game list, keyed by roots + a cheap signature.
// ─────────────────────────────────────────────────────────────────────────────

const CACHE_VERSION: u32 = 1;
/// If a root has at most this many subdirs, the staleness signature also samples
/// one level deeper (so games added inside a small set of letter buckets are
/// detected). Above it, only the root listing is signed (keeps huge
/// folder-per-game libraries fast). Deeper additions need a manual Refresh.
const SIG_DEEP_LIMIT: usize = 64;

#[derive(Serialize, Deserialize)]
struct LibraryCache {
    version: u32,
    host: String,
    roots: Vec<String>,
    signature: String,
    layout_label: String,
    games: Vec<GameEntry>,
}

fn cache_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ultimate64-manager").join("game_library_cache.json"))
}

fn load_cache() -> Option<LibraryCache> {
    let path = cache_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    let cache: LibraryCache = serde_json::from_str(&data).ok()?;
    (cache.version == CACHE_VERSION).then_some(cache)
}

fn save_cache(cache: &LibraryCache) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, json);
    }
}

/// A cheap fingerprint of the library used to auto-invalidate the cache: the
/// immediate listing of each root, plus one level of subdir listings when a
/// root has few subdirs. Sensitive to added/removed/renamed/resized entries at
/// those levels. Far cheaper than a full walk.
async fn compute_signature(host: &str, roots: &[String], password: &Option<String>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for root in roots {
        let local = root_is_local(root);
        let Ok(files) = list_dir(local, host, root, password).await else {
            parts.push(format!("{root}#ERR"));
            continue;
        };
        let mut entries: Vec<String> = files
            .iter()
            .map(|f| format!("{}|{}|{}", f.name, f.is_dir, f.size))
            .collect();
        let subdirs: Vec<&RemoteFileEntry> = files.iter().filter(|f| f.is_dir).collect();
        if subdirs.len() <= SIG_DEEP_LIMIT {
            for d in &subdirs {
                if let Ok(sub) = list_dir(local, host, &d.path, password).await {
                    for f in &sub {
                        entries.push(format!("{}/{}|{}|{}", d.name, f.name, f.is_dir, f.size));
                    }
                }
            }
        }
        entries.sort();
        parts.push(format!("{root}#{}", entries.join(",")));
    }
    format!("{:x}", md5::compute(parts.join(";;")))
}

/// Entry point used by Toggle/Refresh: return the cached game list when the
/// signature still matches, otherwise scan and refresh the cache. `force`
/// (Refresh) always re-scans.
async fn load_or_scan(
    host: Option<String>,
    roots: Vec<String>,
    password: Option<String>,
    force: bool,
) -> Result<ScanResult, String> {
    let host = host.unwrap_or_default();
    let signature = compute_signature(&host, &roots, &password).await;

    if !force {
        if let Some(cache) = load_cache() {
            if cache.host == host && cache.roots == roots && cache.signature == signature {
                log::info!("Game Mode: cache hit ({} games)", cache.games.len());
                return Ok(ScanResult {
                    games: cache.games,
                    layout_label: if cache.layout_label.is_empty() {
                        "cached".to_string()
                    } else {
                        format!("{} · cached", cache.layout_label)
                    },
                });
            }
        }
    }

    let scan = enumerate_library(host.clone(), roots.clone(), password).await?;
    save_cache(&LibraryCache {
        version: CACHE_VERSION,
        host,
        roots,
        signature,
        layout_label: scan.layout_label.clone(),
        games: scan.games.clone(),
    });
    Ok(scan)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Library scan
// ─────────────────────────────────────────────────────────────────────────────

/// Scan every root, detecting each one's layout and collecting games. Each root
/// is local or device (decided by [`root_is_local`]). Errors are only fatal if
/// no root yields anything.
async fn enumerate_library(
    host: String,
    roots: Vec<String>,
    password: Option<String>,
) -> Result<ScanResult, String> {
    let mut games: Vec<GameEntry> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut capped = false;

    for root in &roots {
        let local = root_is_local(root);
        let files = match list_dir(local, &host, root, &password).await {
            Ok(f) => f,
            Err(e) => {
                errors.push(format!("{}: {}", path_basename(root), e));
                continue;
            }
        };

        let art = fetch_art_index(local, &host, root, &password).await;
        let layout = detect_layout(local, &host, &files, &password).await;
        labels.push(layout.label().to_string());
        log::info!(
            "Game Mode: {} ({}) → layout '{}'",
            path_basename(root),
            if local { "local" } else { "device" },
            layout.label()
        );

        match layout {
            LibraryLayout::FlatFiles => emit_flat(local, &files, &art, &mut games),
            LibraryLayout::FileBuckets | LibraryLayout::GameFolders | LibraryLayout::Grouped => {
                walk(
                    local,
                    &host,
                    root,
                    &files,
                    &password,
                    &art,
                    &mut games,
                    &mut capped,
                )
                .await;
            }
        }
        if games.len() >= MAX_GAMES {
            capped = true;
            break;
        }
    }

    if capped {
        log::warn!("Game Mode: enumeration capped at {} games", games.len());
    }
    if games.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }

    games.sort_by_key(|g| g.title.to_lowercase());
    let layout_label = {
        let mut uniq = labels;
        uniq.dedup();
        uniq.join(", ")
    };
    Ok(ScanResult {
        games,
        layout_label,
    })
}

/// Detect a root's layout from its own listing plus a small sample of subdirs.
async fn detect_layout(
    local: bool,
    host: &str,
    root_files: &[RemoteFileEntry],
    password: &Option<String>,
) -> LibraryLayout {
    let runnables = runnable_files(root_files);
    let subdirs: Vec<&RemoteFileEntry> = root_files.iter().filter(|f| f.is_dir).collect();

    if runnables.len() >= 2 && runnables.len() >= subdirs.len() {
        return LibraryLayout::FlatFiles;
    }
    if subdirs.is_empty() {
        return if runnables.is_empty() {
            LibraryLayout::Grouped
        } else {
            LibraryLayout::FlatFiles
        };
    }

    let (mut buckets, mut folders, mut groups) = (0usize, 0usize, 0usize);
    for d in subdirs.iter().take(DETECT_SAMPLE) {
        let Ok(sub) = list_dir(local, host, &d.path, password).await else {
            continue;
        };
        let r = runnable_files(&sub).len();
        let s = sub.iter().filter(|f| f.is_dir).count();
        match classify_folder(r, s) {
            FolderKind::Flat => buckets += 1,
            FolderKind::GameFolder => folders += 1,
            FolderKind::Group => groups += 1,
        }
    }

    if buckets >= folders && buckets >= groups && buckets > 0 {
        LibraryLayout::FileBuckets
    } else if folders >= groups && folders > 0 {
        LibraryLayout::GameFolders
    } else {
        LibraryLayout::Grouped
    }
}

/// Emit one game per runnable file directly in the folder.
fn emit_flat(local: bool, files: &[RemoteFileEntry], art: &ArtIndex, out: &mut Vec<GameEntry>) {
    for f in runnable_files(files) {
        let base = stem_lower(&f.name);
        let (cover, shot) = resolve_file_art(&base, files, art);
        let title = nice_title(&f.name);
        out.push(GameEntry {
            letter: leading_letter(&title),
            title,
            run_path: Some(f.path.clone()),
            cover_path: cover,
            shot_path: shot,
            key: f.path.clone(),
            local,
        });
    }
}

/// Breadth-first walk that classifies each folder: flat → file games (no
/// recurse), single runnable → one game folder, no runnables → recurse.
#[allow(clippy::too_many_arguments)]
async fn walk(
    local: bool,
    host: &str,
    root: &str,
    root_files: &[RemoteFileEntry],
    password: &Option<String>,
    art: &ArtIndex,
    out: &mut Vec<GameEntry>,
    capped: &mut bool,
) {
    let mut queue: VecDeque<(String, u32, Option<Vec<RemoteFileEntry>>)> = VecDeque::new();
    queue.push_back((root.to_string(), 0, Some(root_files.to_vec())));
    let mut dirs_visited = 0usize;

    while let Some((dir, depth, preloaded)) = queue.pop_front() {
        if out.len() >= MAX_GAMES || dirs_visited >= MAX_DIRS {
            *capped = true;
            break;
        }
        dirs_visited += 1;

        let files = match preloaded {
            Some(f) => f,
            None => match list_dir(local, host, &dir, password).await {
                Ok(f) => f,
                Err(_) => continue,
            },
        };

        let runnables = runnable_files(&files);
        let subdirs: Vec<&RemoteFileEntry> = files.iter().filter(|f| f.is_dir).collect();

        match classify_folder(runnables.len(), subdirs.len()) {
            FolderKind::Flat => {
                for f in &runnables {
                    let base = stem_lower(&f.name);
                    let (cover, shot) = resolve_file_art(&base, &files, art);
                    let title = nice_title(&f.name);
                    out.push(GameEntry {
                        letter: leading_letter(&title),
                        title,
                        run_path: Some(f.path.clone()),
                        cover_path: cover,
                        shot_path: shot,
                        key: f.path.clone(),
                        local,
                    });
                }
            }
            FolderKind::GameFolder => {
                let primary = primary_runnable(&files);
                let base = primary.map(|f| stem_lower(&f.name));
                let (cover, shot) = resolve_folder_art(base.as_deref(), &files, art);
                let title = path_basename(&dir);
                out.push(GameEntry {
                    letter: leading_letter(&title),
                    title,
                    run_path: primary.map(|f| f.path.clone()),
                    cover_path: cover,
                    shot_path: shot,
                    key: dir.clone(),
                    local,
                });
            }
            FolderKind::Group => {
                if depth < MAX_DEPTH {
                    for d in &subdirs {
                        queue.push_back((d.path.clone(), depth + 1, None));
                    }
                }
            }
        }
    }
}

/// Art for a file-game: a sibling image with the same stem wins, else central
/// art by basename.
fn resolve_file_art(
    base: &str,
    files: &[RemoteFileEntry],
    art: &ArtIndex,
) -> (Option<String>, Option<String>) {
    let sibling = files
        .iter()
        .find(|im| {
            !im.is_dir && crate::file_types::is_image_file(&im.name) && stem_lower(&im.name) == base
        })
        .map(|im| im.path.clone());
    if art.is_empty() {
        return (sibling, None);
    }
    let (c_cover, c_shot) = art.lookup(base);
    (sibling.or(c_cover), c_shot)
}

/// Art for a folder-game: sibling images (cover + a distinct screenshot) win,
/// else central art by the primary file's basename.
fn resolve_folder_art(
    base: Option<&str>,
    files: &[RemoteFileEntry],
    art: &ArtIndex,
) -> (Option<String>, Option<String>) {
    let sib_cover = pick_cover(files, base);
    let sib_shot = pick_screenshot(files, sib_cover.as_deref());
    let (c_cover, c_shot) = base.map(|b| art.lookup(b)).unwrap_or((None, None));
    (sib_cover.or(c_cover), sib_shot.or(c_shot))
}

/// Probe the candidate central art folders under `root` and index them by
/// basename. Missing folders are skipped silently.
async fn fetch_art_index(
    local: bool,
    host: &str,
    root: &str,
    password: &Option<String>,
) -> ArtIndex {
    let mut idx = ArtIndex::default();
    for rel in ART_FOLDER_CANDIDATES {
        let dir = format!("{}/{}", root.trim_end_matches('/'), rel);
        let Ok(files) = list_dir(local, host, &dir, password).await else {
            continue;
        };
        let target = if rel.ends_with("LoadingScreens") {
            &mut idx.loadingscreens
        } else if rel.ends_with("Screenshots") {
            &mut idx.screenshots
        } else {
            &mut idx.images
        };
        for f in files {
            if !f.is_dir && crate::file_types::is_image_file(&f.name) {
                target.entry(stem_lower(&f.name)).or_insert(f.path);
            }
        }
    }
    idx
}

/// Download a game's already-resolved cover + screenshot from its source.
/// Best-effort: a missing/failed image just yields `None`.
async fn download_game_art(
    local: bool,
    host: String,
    cover_path: Option<String>,
    shot_path: Option<String>,
    password: Option<String>,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let cover = match cover_path {
        Some(p) => read_image(local, &host, &p, &password).await,
        None => None,
    };
    let shot = match shot_path {
        Some(p) => read_image(local, &host, &p, &password).await,
        None => None,
    };
    (cover, shot)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> RemoteFileEntry {
        RemoteFileEntry {
            name: name.to_string(),
            is_dir,
            size: 0,
            path: format!("/games/{}", name),
        }
    }

    #[test]
    fn classify_folder_picks_kind() {
        assert_eq!(classify_folder(2145, 5), FolderKind::Flat); // OneLoad64 root
        assert_eq!(classify_folder(30, 0), FolderKind::Flat); // letter bucket
        assert_eq!(classify_folder(1, 0), FolderKind::GameFolder);
        assert_eq!(classify_folder(0, 26), FolderKind::Group);
        assert_eq!(classify_folder(2, 0), FolderKind::Flat);
    }

    #[test]
    fn leading_letter_folds_digits_and_symbols() {
        assert_eq!(leading_letter("Arkanoid"), 'A');
        assert_eq!(leading_letter("wizball"), 'W');
        assert_eq!(leading_letter("1942"), '#');
        assert_eq!(leading_letter("$1,000,000"), '#');
    }

    #[test]
    fn file_ext_and_nice_title() {
        assert_eq!(file_ext("Game.CRT"), "crt");
        assert_eq!(file_ext("noext"), "");
        assert_eq!(nice_title("THE_LAST_NINJA.prg"), "THE LAST NINJA");
        assert_eq!(nice_title("Commando"), "Commando");
    }

    #[test]
    fn central_art_matches_by_basename_with_parens() {
        let mut idx = ArtIndex::default();
        idx.screenshots.insert(
            stem_lower("1942 (Music v1).png"),
            "/root/Extras/Images/Screenshots/1942 (Music v1).png".to_string(),
        );
        idx.loadingscreens.insert(
            stem_lower("1942 (Music v1).png"),
            "/root/Extras/Images/LoadingScreens/1942 (Music v1).png".to_string(),
        );
        let base = stem_lower("1942 (Music v1).crt");
        let (cover, shot) = idx.lookup(&base);
        assert!(cover.unwrap().contains("LoadingScreens"));
        assert!(shot.unwrap().contains("Screenshots"));
    }

    #[test]
    fn flat_file_art_prefers_sibling_over_central() {
        let files = vec![entry("elite.crt", false), entry("elite.jpg", false)];
        let mut idx = ArtIndex::default();
        idx.screenshots
            .insert("elite".to_string(), "/central/elite.png".to_string());
        let (cover, _shot) = resolve_file_art("elite", &files, &idx);
        assert_eq!(cover.as_deref(), Some("/games/elite.jpg"));
    }

    #[test]
    fn pick_cover_prefers_stem_match() {
        let files = vec![
            entry("cover.jpg", false),
            entry("game.png", false),
            entry("game.prg", false),
        ];
        assert_eq!(
            pick_cover(&files, Some("game")).as_deref(),
            Some("/games/game.png")
        );
    }

    #[test]
    fn pick_screenshot_skips_cover_and_prefers_hints() {
        let files = vec![
            entry("cover.jpg", false),
            entry("aaa.png", false),
            entry("ingame.png", false),
        ];
        let cover = pick_cover(&files, None);
        assert_eq!(
            pick_screenshot(&files, cover.as_deref()).as_deref(),
            Some("/games/ingame.png")
        );
    }

    #[test]
    fn primary_runnable_prefers_prg_then_crt_then_disk() {
        let all = vec![
            entry("game.d64", false),
            entry("game.crt", false),
            entry("loader.prg", false),
        ];
        assert_eq!(
            primary_runnable(&all).map(|f| f.name.as_str()),
            Some("loader.prg")
        );
        let crt_disk = vec![entry("game.d64", false), entry("game.crt", false)];
        assert_eq!(
            primary_runnable(&crt_disk).map(|f| f.name.as_str()),
            Some("game.crt")
        );
        let none = vec![entry("readme.txt", false)];
        assert!(primary_runnable(&none).is_none());
    }

    /// End-to-end check of the classification + art resolution against a real
    /// folder-per-game fixture built from OneLoad64 (see the `make_gamefolders`
    /// helper). Ignored by default — it reads a local path that only exists on
    /// the author's machine, and returns early elsewhere.
    #[test]
    #[ignore = "requires local OneLoad64-GameFolders-Test fixture"]
    fn oneload_gamefolders_fixture_scans_correctly() {
        use std::fs;
        use std::path::Path;

        let root = "/Users/marcin/Downloads/OneLoad64-GameFolders-Test";
        let Ok(read) = fs::read_dir(root) else {
            eprintln!("fixture missing — skipping");
            return;
        };

        let list = |dir: &Path| -> Vec<RemoteFileEntry> {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    RemoteFileEntry {
                        name: e.file_name().to_string_lossy().to_string(),
                        is_dir,
                        size: 0,
                        path: e.path().to_string_lossy().to_string(),
                    }
                })
                .collect()
        };

        let subdirs: Vec<_> = read
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        assert!(
            subdirs.len() > 2000,
            "expected the ~2145-folder fixture, got {}",
            subdirs.len()
        );

        // The root is a grouping folder (no runnables, many subdirs) → the
        // walker recurses into each game folder.
        let root_files = list(Path::new(root));
        let root_subdirs = root_files.iter().filter(|f| f.is_dir).count();
        assert_eq!(
            classify_folder(runnable_files(&root_files).len(), root_subdirs),
            FolderKind::Group
        );

        let (mut games, mut with_cover) = (0usize, 0usize);
        for d in &subdirs {
            let files = list(d);
            let runs = runnable_files(&files);
            assert_eq!(
                classify_folder(runs.len(), 0),
                FolderKind::GameFolder,
                "{:?}",
                d
            );
            let primary = primary_runnable(&files);
            assert!(primary.is_some(), "no runnable in {:?}", d);
            let stem = primary.map(|f| stem_lower(&f.name));
            if pick_cover(&files, stem.as_deref()).is_some() {
                with_cover += 1;
            }
            games += 1;
        }
        assert_eq!(games, subdirs.len());
        assert_eq!(
            with_cover, games,
            "every folder should resolve the empty.jpg cover"
        );
        println!("fixture OK: {games} game folders, all GameFolder + cover resolved");
    }

    /// Confirms the original bug is fixed: the flat OneLoad64 root
    /// (~2145 `.crt` + a few support subfolders, no sibling images) is detected
    /// as `FlatFiles` and emits every game — not one entry for the collection.
    #[test]
    #[ignore = "requires local OneLoad64-Games-Collection-v5 fixture"]
    fn oneload_flat_root_emits_all_games() {
        use std::fs;
        use std::path::Path;

        let root = "/Users/marcin/Downloads/OneLoad64-Games-Collection-v5";
        let Ok(_) = fs::read_dir(root) else {
            eprintln!("fixture missing — skipping");
            return;
        };
        let files: Vec<RemoteFileEntry> = fs::read_dir(Path::new(root))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| RemoteFileEntry {
                is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                name: e.file_name().to_string_lossy().to_string(),
                size: 0,
                path: e.path().to_string_lossy().to_string(),
            })
            .collect();

        let runnables = runnable_files(&files).len();
        let subdirs = files.iter().filter(|f| f.is_dir).count();
        // Detection rule for a self-flat root (mirrors `detect_layout`).
        assert!(
            runnables >= 2 && runnables >= subdirs,
            "flat root should win over support subdirs ({runnables} runnables, {subdirs} subdirs)"
        );
        assert_eq!(classify_folder(runnables, subdirs), FolderKind::Flat);

        let mut games = Vec::new();
        emit_flat(true, &files, &ArtIndex::default(), &mut games);
        assert!(
            games.len() > 2000,
            "flat scan should list every game, got {}",
            games.len()
        );
        assert!(games.iter().all(|g| g.run_path.is_some()));
        println!("flat OneLoad64: emitted {} games", games.len());
    }

    /// Runs the full local enumeration against the folder-per-game fixture:
    /// detects `GameFolders`, marks every game `local`, and resolves the sibling
    /// `empty.jpg` as cover.
    #[test]
    #[ignore = "requires local OneLoad64-GameFolders-Test fixture"]
    fn local_gamefolders_enumerates_end_to_end() {
        let root = "/Users/marcin/Downloads/OneLoad64-GameFolders-Test";
        if !std::path::Path::new(root).is_dir() {
            eprintln!("fixture missing — skipping");
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let scan = rt
            .block_on(enumerate_library(
                String::new(),
                vec![root.to_string()],
                None,
            ))
            .expect("scan");
        assert!(scan.games.len() > 2000, "got {}", scan.games.len());
        assert!(scan.games.iter().all(|g| g.local));
        assert!(scan.games.iter().all(|g| g.run_path.is_some()));
        assert!(
            scan.games.iter().all(|g| g.cover_path.is_some()),
            "every folder-game should resolve its empty.jpg sibling"
        );
        println!(
            "local game folders: {} games, layout '{}'",
            scan.games.len(),
            scan.layout_label
        );
    }

    /// Runs the full local enumeration against the flat OneLoad64 collection:
    /// detects `FlatFiles`, lists every `.crt`, and pulls box art from the
    /// central `Extras/Images/*` folders (no sibling images at the root).
    #[test]
    #[ignore = "requires local OneLoad64-Games-Collection-v5 fixture"]
    fn local_flat_enumerates_with_central_art() {
        let root = "/Users/marcin/Downloads/OneLoad64-Games-Collection-v5";
        if !std::path::Path::new(root).is_dir() {
            eprintln!("fixture missing — skipping");
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let scan = rt
            .block_on(enumerate_library(
                String::new(),
                vec![root.to_string()],
                None,
            ))
            .expect("scan");
        assert!(scan.games.len() > 2000, "got {}", scan.games.len());
        assert!(scan.games.iter().all(|g| g.local));
        assert!(scan.layout_label.contains("flat"));
        let with_cover = scan.games.iter().filter(|g| g.cover_path.is_some()).count();
        assert!(
            with_cover > scan.games.len() / 2,
            "expected central art for most games, got {}/{}",
            with_cover,
            scan.games.len()
        );
        println!(
            "local flat: {} games, {} with central art, layout '{}'",
            scan.games.len(),
            with_cover,
            scan.layout_label
        );
    }

    #[test]
    fn library_cache_serde_roundtrips() {
        let cache = LibraryCache {
            version: CACHE_VERSION,
            host: "10.0.0.5".to_string(),
            roots: vec!["/Usb0/Games".to_string(), "/Users/me/Games".to_string()],
            signature: "abc123".to_string(),
            layout_label: "flat files".to_string(),
            games: vec![GameEntry {
                title: "Arkanoid".to_string(),
                run_path: Some("/Usb0/Games/Arkanoid.crt".to_string()),
                cover_path: Some("/central/Arkanoid.png".to_string()),
                shot_path: None,
                key: "/Usb0/Games/Arkanoid.crt".to_string(),
                letter: 'A',
                local: false,
            }],
        };
        let json = serde_json::to_string(&cache).unwrap();
        let back: LibraryCache = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, CACHE_VERSION);
        assert_eq!(back.roots, cache.roots);
        assert_eq!(back.games.len(), 1);
        assert_eq!(back.games[0].title, "Arkanoid");
        assert_eq!(back.games[0].letter, 'A');
        assert!(!back.games[0].local);
    }

    #[test]
    fn headers_before_counts_section_changes() {
        let mk = |title: &str| GameEntry {
            title: title.to_string(),
            run_path: None,
            cover_path: None,
            shot_path: None,
            key: title.to_string(),
            letter: leading_letter(title),
            local: false,
        };
        let games = vec![mk("Alpha"), mk("Arc"), mk("Beta"), mk("Cyan")];
        assert_eq!(headers_before(&games, 0), 1); // A
        assert_eq!(headers_before(&games, 1), 1); // still A
        assert_eq!(headers_before(&games, 2), 2); // A, B
        assert_eq!(headers_before(&games, 3), 3); // A, B, C
    }
}
