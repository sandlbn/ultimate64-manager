//! Iced UI for the Assembly64 browser tab.
//!
//! Replaces the old CSDB browser. The tab is structured as:
//! - **Search bar** (top): plain text + Type / Source / Rating / Recency /
//!   Sort dropdowns. The composed AQL is hidden by default; flip the
//!   "Advanced query" toggle to see and edit it.
//! - **Results list**: paged search hits, infinite-scroll style ("Load more").
//! - **Entry detail view**: file list + screenshot panel + Run/Mount/Download.
//! - **ZIP contents view**: same shape as the entry view but for files
//!   extracted from a downloaded ZIP.
//! - **Favorites view**: starred entries persisted to `assembly64.json`.
//!
//! Persistence (favorites + saved searches) lives in a small per-tab JSON
//! file alongside `settings.json` — no changes to the global settings module.

use iced::{
    widget::{
        button, checkbox, column, container, pick_list, row, rule, scrollable, text, text_input,
        tooltip, Column, Space,
    },
    Element, Length, Task,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::archive::{extract_zip_to_dir, runnable_extracted_files, ExtractedFile, ExtractedZip};
use crate::assembly64::{
    rating_stars, AsmEntry, AsmFile, Assembly64Client, AssemblyError, CategoryRegistry, Choice,
    Presets, RatingFilter, RecencyFilter, SearchForm, SortOrder, DEFAULT_PAGE_SIZE,
};

const HTTP_TIMEOUT_SECS: u64 = 30;
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
const NAME_DISPLAY_CAP: usize = 45;
const GROUP_DISPLAY_CAP: usize = 24;
const USER_AGENT_FALLBACK: &str = "ultimate64-manager";

// -----------------------------------------------------------------------------
// Sub-types reused inside the browser
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOption {
    A,
    B,
}

impl DriveOption {
    pub fn to_drive_string(&self) -> String {
        match self {
            DriveOption::A => "a".to_string(),
            DriveOption::B => "b".to_string(),
        }
    }

    pub fn device_number(&self) -> &'static str {
        match self {
            DriveOption::A => "8",
            DriveOption::B => "9",
        }
    }

    pub fn all() -> Vec<DriveOption> {
        vec![DriveOption::A, DriveOption::B]
    }
}

impl std::fmt::Display for DriveOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriveOption::A => write!(f, "Drive A (8)"),
            DriveOption::B => write!(f, "Drive B (9)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

impl std::fmt::Display for MountMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountMode::ReadOnly => write!(f, "RO"),
            MountMode::ReadWrite => write!(f, "RW"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFilter {
    All,
    Runnable,
    Disk,
    Program,
    Music,
    Archive,
}

impl std::fmt::Display for FileFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FileFilter::All => "All Files",
            FileFilter::Runnable => "Runnable",
            FileFilter::Disk => "Disk Images",
            FileFilter::Program => "Programs",
            FileFilter::Music => "Music (SID)",
            FileFilter::Archive => "Archives",
        };
        f.write_str(s)
    }
}

impl FileFilter {
    fn all() -> Vec<FileFilter> {
        vec![
            FileFilter::All,
            FileFilter::Runnable,
            FileFilter::Disk,
            FileFilter::Program,
            FileFilter::Music,
            FileFilter::Archive,
        ]
    }

    fn matches(&self, ext: &str) -> bool {
        match self {
            FileFilter::All => true,
            FileFilter::Runnable => {
                crate::file_types::is_runnable(ext) || crate::file_types::is_zip_file(ext)
            }
            FileFilter::Disk => crate::file_types::is_disk_image(ext),
            FileFilter::Program => matches!(ext, "prg" | "crt"),
            FileFilter::Music => ext == "sid",
            FileFilter::Archive => crate::file_types::is_zip_file(ext),
        }
    }
}

/// Action waiting on the drive-enable confirmation dialog.
#[derive(Debug, Clone)]
pub enum AsmPendingAction {
    RunFile(u64),
    MountFile(u64, MountMode),
    RunExtractedFile(usize),
    MountExtractedFile(usize, MountMode),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewState {
    Results,
    EntryDetails,
    ZipContents,
    Favorites,
}

// -----------------------------------------------------------------------------
// Persistence: favorites + saved searches
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorite {
    pub item_id: String,
    pub category_id: u16,
    pub name: String,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    /// Composed AQL string captured at save time.
    pub aql: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    favorites: Vec<Favorite>,
    #[serde(default)]
    saved_searches: Vec<SavedSearch>,
}

fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("ultimate64-manager"))
}

fn persistence_path() -> Option<PathBuf> {
    Some(config_dir()?.join("assembly64.json"))
}

fn presets_cache_path() -> Option<PathBuf> {
    Some(config_dir()?.join("assembly64_presets.json"))
}

fn categories_cache_path() -> Option<PathBuf> {
    Some(config_dir()?.join("assembly64_categories.json"))
}

fn load_persisted() -> PersistedState {
    let path = match persistence_path() {
        Some(p) => p,
        None => return PersistedState::default(),
    };
    if !path.exists() {
        return PersistedState::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    }
}

fn save_persisted(state: &PersistedState) {
    let Some(path) = persistence_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, s);
    }
}

fn load_cached_presets() -> Option<Presets> {
    let path = presets_cache_path()?;
    let s = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_cached_presets(presets: &Presets) {
    let Some(path) = presets_cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(presets) {
        let _ = std::fs::write(&path, s);
    }
}

/// Cache file shape for the category registry — store the raw entry list and
/// rebuild the HashMap at load time.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CategoryCache {
    entries: Vec<crate::assembly64::CategoryInfo>,
}

fn load_cached_categories() -> Option<CategoryRegistry> {
    let path = categories_cache_path()?;
    let s = std::fs::read_to_string(&path).ok()?;
    let cache: CategoryCache = serde_json::from_str(&s).ok()?;
    Some(CategoryRegistry::new(cache.entries))
}

fn save_cached_categories(registry: &CategoryRegistry) {
    let Some(path) = categories_cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = CategoryCache {
        entries: registry.entries().cloned().collect(),
    };
    if let Ok(s) = serde_json::to_string_pretty(&cache) {
        let _ = std::fs::write(&path, s);
    }
}

// -----------------------------------------------------------------------------
// Messages
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Assembly64BrowserMessage {
    // Server-driven preset / category metadata refresh.
    RefreshPresets,
    PresetsLoaded(Result<Presets, String>),
    CategoriesLoaded(Result<CategoryRegistry, String>),

    // Search form
    FreeTextChanged(String),
    TypeFilterChanged(Choice),
    SourceFilterChanged(Choice),
    RatingFilterChanged(RatingFilter),
    RecencyFilterChanged(RecencyFilter),
    SortChanged(SortOrder),
    SearchSubmit,
    /// Clear all form filters and re-run as a "show latest entries" query.
    ResetAndShowLatest,
    LoadMore,
    SearchCompleted(Result<SearchResultsBatch, String>),

    // Advanced query disclosure
    ToggleAdvancedQuery(bool),

    // Saved searches
    NewSavedSearchNameChanged(String),
    SaveCurrentSearch,
    ApplySavedSearch(String),  // by name
    RemoveSavedSearch(String), // by name

    // Favorites
    ShowFavorites,
    ToggleFavorite(String, u16, String, Option<String>),
    OpenFavorite(String, u16, String),

    // External link
    OpenInBrowser(String),

    // Entry selection
    SelectEntry(String, u16), // item_id, category_id
    EntryFilesLoaded(Result<EntryFilesBatch, String>),
    BackToList,

    // Screenshot
    ScreenshotLoaded(Result<Option<Vec<u8>>, String>),

    // Files
    SelectFile(u64),
    DownloadFile(u64),
    DownloadCompleted(Result<PathBuf, String>),
    RunFile(u64),
    DoRunFile(u64),
    RunFileCompleted(Result<String, String>),
    MountFile(u64, MountMode),
    DoMountFile(u64, MountMode),
    MountCompleted(Result<String, String>),

    // Drive enable dialog
    CheckDriveBeforeAction(AsmPendingAction),
    DriveCheckComplete(Result<bool, String>, AsmPendingAction),
    ConfirmEnableDrive,
    CancelEnableDrive,
    EnableDriveComplete(Result<(), String>),

    // Drive selection / file filter
    DriveSelected(DriveOption),
    FilterChanged(FileFilter),

    // ZIP
    ExtractZip(u64),
    ZipExtracted(Result<ExtractedZip, String>),
    SelectExtractedFile(usize),
    RunExtractedFile(usize),
    DoRunExtractedFile(usize),
    RunExtractedFileCompleted(Result<String, String>),
    MountExtractedFile(usize, MountMode),
    DoMountExtractedFile(usize, MountMode),
    MountExtractedFileCompleted(Result<String, String>),
    CloseZipView,
}

/// One page of search results plus the offset they correspond to (so we can
/// decide whether to append vs. replace, and where to start the next page).
#[derive(Debug, Clone)]
pub struct SearchResultsBatch {
    pub query: String,
    pub offset: u32,
    pub entries: Vec<AsmEntry>,
}

#[derive(Debug, Clone)]
pub struct EntryFilesBatch {
    pub item_id: String,
    pub category_id: u16,
    pub files: Vec<AsmFile>,
}

// -----------------------------------------------------------------------------
// State
// -----------------------------------------------------------------------------

pub struct Assembly64Browser {
    view_state: ViewState,

    // Search form + results
    search_form: SearchForm,
    last_query: String,
    next_offset: u32,
    has_more: bool,
    results: Vec<AsmEntry>,
    show_advanced_query: bool,

    // Saved searches + favorites
    favorites: Vec<Favorite>,
    saved_searches: Vec<SavedSearch>,
    new_saved_search_name: String,

    // Selected entry / files
    selected_entry: Option<AsmEntry>,
    entry_files: Vec<AsmFile>,
    selected_file_id: Option<u64>,

    // ZIP
    extracted_zip: Option<ExtractedZip>,
    selected_extracted_file_index: Option<usize>,

    // Drive
    selected_drive: DriveOption,
    drive_enable_dialog: Option<(DriveOption, AsmPendingAction)>,

    file_filter: FileFilter,

    // Server-driven dropdown contents and id→label map. Populated from
    // disk cache on startup, refreshed from the API on first tab open.
    presets: Presets,
    category_registry: CategoryRegistry,
    presets_loaded_from_server: bool,

    // Status
    status_message: Option<String>,
    is_loading: bool,

    // Screenshot
    screenshot_handle: Option<iced::widget::image::Handle>,
    screenshot_loading: bool,

    // Disk
    download_dir: PathBuf,

    user_agent: String,
}

impl Default for Assembly64Browser {
    fn default() -> Self {
        Self::new()
    }
}

impl Assembly64Browser {
    pub fn new() -> Self {
        let download_dir = dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("ultimate64-manager")
            .join("Assembly64");

        let persisted = load_persisted();
        // Prefer the on-disk cache; fall back to the hardcoded baseline so
        // the dropdowns are usable on the very first run.
        let presets = load_cached_presets().unwrap_or_else(Presets::baseline);
        let category_registry = load_cached_categories().unwrap_or_default();

        let user_agent = format!("{}/{}", USER_AGENT_FALLBACK, env!("CARGO_PKG_VERSION"));

        Self {
            view_state: ViewState::Results,
            search_form: SearchForm::default(),
            last_query: String::new(),
            next_offset: 0,
            has_more: false,
            results: Vec::new(),
            show_advanced_query: false,
            favorites: persisted.favorites,
            saved_searches: persisted.saved_searches,
            new_saved_search_name: String::new(),
            selected_entry: None,
            entry_files: Vec::new(),
            selected_file_id: None,
            extracted_zip: None,
            selected_extracted_file_index: None,
            selected_drive: DriveOption::A,
            drive_enable_dialog: None,
            file_filter: FileFilter::Runnable,
            presets,
            category_registry,
            presets_loaded_from_server: false,
            status_message: None,
            is_loading: false,
            screenshot_handle: None,
            screenshot_loading: false,
            download_dir,
            user_agent,
        }
    }

    pub fn has_content(&self) -> bool {
        !self.results.is_empty()
    }

    fn persist(&self) {
        let state = PersistedState {
            favorites: self.favorites.clone(),
            saved_searches: self.saved_searches.clone(),
        };
        save_persisted(&state);
    }

    fn make_client(&self) -> Result<Assembly64Client, String> {
        Assembly64Client::with_defaults(&self.user_agent).map_err(|e| e.to_string())
    }

    // -------------------------------------------------------------------------
    // update
    // -------------------------------------------------------------------------

    pub fn update(
        &mut self,
        message: Assembly64BrowserMessage,
        connection: Option<Arc<Mutex<Rest>>>,
        host: Option<String>,
        password: Option<String>,
    ) -> Task<Assembly64BrowserMessage> {
        use Assembly64BrowserMessage as M;

        match message {
            // ── server-driven preset / category metadata ────────────────
            M::RefreshPresets => {
                if self.presets_loaded_from_server {
                    return Task::none();
                }
                let user_agent = self.user_agent.clone();
                let presets_task = Task::perform(
                    async move {
                        let client = Assembly64Client::with_defaults(&user_agent)
                            .map_err(|e| e.to_string())?;
                        client.presets().await.map_err(|e| e.to_string())
                    },
                    M::PresetsLoaded,
                );
                let user_agent = self.user_agent.clone();
                let categories_task = Task::perform(
                    async move {
                        let client = Assembly64Client::with_defaults(&user_agent)
                            .map_err(|e| e.to_string())?;
                        client.category_registry().await.map_err(|e| e.to_string())
                    },
                    M::CategoriesLoaded,
                );
                Task::batch([presets_task, categories_task])
            }
            M::PresetsLoaded(result) => {
                match result {
                    Ok(presets) => {
                        // Migrate the user's currently selected source/type
                        // by aql_key so a refresh doesn't clobber a chosen
                        // filter (server might rename labels).
                        if !self.search_form.source_filter.is_any() {
                            if let Some(c) = presets
                                .sources
                                .iter()
                                .find(|c| c.aql_key == self.search_form.source_filter.aql_key)
                            {
                                self.search_form.source_filter = c.clone();
                            }
                        }
                        if !self.search_form.type_filter.is_any() {
                            if let Some(c) = presets
                                .types
                                .iter()
                                .find(|c| c.aql_key == self.search_form.type_filter.aql_key)
                            {
                                self.search_form.type_filter = c.clone();
                            }
                        }
                        save_cached_presets(&presets);
                        self.presets = presets;
                        self.presets_loaded_from_server = true;
                    }
                    Err(e) => {
                        // Failure is non-fatal — we keep the cached / baseline
                        // dropdowns and let the user retry next session.
                        log::warn!("Assembly64 presets refresh failed: {}", e);
                    }
                }
                Task::none()
            }
            M::CategoriesLoaded(result) => {
                match result {
                    Ok(registry) => {
                        save_cached_categories(&registry);
                        self.category_registry = registry;
                    }
                    Err(e) => {
                        log::warn!("Assembly64 categories refresh failed: {}", e);
                    }
                }
                Task::none()
            }

            // ── search form fields ──────────────────────────────────────
            M::FreeTextChanged(v) => {
                self.search_form.free_text = v;
                Task::none()
            }
            M::TypeFilterChanged(v) => {
                self.search_form.type_filter = v;
                Task::none()
            }
            M::SourceFilterChanged(v) => {
                self.search_form.source_filter = v;
                Task::none()
            }
            M::RatingFilterChanged(v) => {
                self.search_form.rating_filter = v;
                Task::none()
            }
            M::RecencyFilterChanged(v) => {
                self.search_form.recency_filter = v;
                Task::none()
            }
            M::SortChanged(v) => {
                self.search_form.sort_order = v;
                Task::none()
            }
            M::ToggleAdvancedQuery(v) => {
                self.show_advanced_query = v;
                Task::none()
            }

            // ── search submit / load more ───────────────────────────────
            M::SearchSubmit => {
                let aql = self.search_form.compose_aql();
                self.last_query = aql.clone();
                self.next_offset = 0;
                self.has_more = false;
                self.results.clear();
                self.view_state = ViewState::Results;
                self.is_loading = true;
                self.status_message = Some("Searching…".to_string());
                self.spawn_search(aql, 0)
            }
            M::ResetAndShowLatest => {
                self.search_form = SearchForm::default();
                Task::done(M::SearchSubmit)
            }
            M::LoadMore => {
                if self.is_loading || !self.has_more {
                    return Task::none();
                }
                let aql = self.last_query.clone();
                let offset = self.next_offset;
                self.is_loading = true;
                self.status_message = Some("Loading more…".to_string());
                self.spawn_search(aql, offset)
            }
            M::SearchCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(batch) => {
                        // Discard out-of-order responses (user may have changed
                        // the query while a request was in flight).
                        if batch.query != self.last_query {
                            return Task::none();
                        }
                        let count = batch.entries.len();
                        self.has_more = count as u32 >= DEFAULT_PAGE_SIZE;
                        self.next_offset = batch.offset + count as u32;
                        if batch.offset == 0 {
                            self.results = batch.entries;
                        } else {
                            self.results.extend(batch.entries);
                        }
                        self.status_message = Some(format!(
                            "{} entries{}",
                            self.results.len(),
                            if self.has_more {
                                " (more available)"
                            } else {
                                ""
                            }
                        ));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Search failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── saved searches ──────────────────────────────────────────
            M::NewSavedSearchNameChanged(v) => {
                self.new_saved_search_name = v;
                Task::none()
            }
            M::SaveCurrentSearch => {
                let name = self.new_saved_search_name.trim().to_string();
                if name.is_empty() {
                    self.status_message = Some("Name the search first".to_string());
                    return Task::none();
                }
                let aql = self.search_form.compose_aql();
                self.saved_searches.retain(|s| s.name != name);
                self.saved_searches.push(SavedSearch { name, aql });
                self.new_saved_search_name.clear();
                self.persist();
                self.status_message = Some("Search saved".to_string());
                Task::none()
            }
            M::ApplySavedSearch(name) => {
                if let Some(s) = self.saved_searches.iter().find(|s| s.name == name) {
                    self.search_form = SearchForm {
                        free_text: s.aql.clone(),
                        ..Default::default()
                    };
                    self.last_query = s.aql.clone();
                    self.next_offset = 0;
                    self.has_more = false;
                    self.results.clear();
                    self.view_state = ViewState::Results;
                    self.is_loading = true;
                    self.status_message = Some(format!("Running '{}'", name));
                    let aql = s.aql.clone();
                    return self.spawn_search(aql, 0);
                }
                Task::none()
            }
            M::RemoveSavedSearch(name) => {
                self.saved_searches.retain(|s| s.name != name);
                self.persist();
                Task::none()
            }

            // ── favorites ───────────────────────────────────────────────
            M::ShowFavorites => {
                self.view_state = ViewState::Favorites;
                Task::none()
            }
            M::ToggleFavorite(item_id, category_id, name, group) => {
                let key = (item_id.clone(), category_id);
                let exists = self
                    .favorites
                    .iter()
                    .any(|f| f.item_id == key.0 && f.category_id == key.1);
                if exists {
                    self.favorites
                        .retain(|f| !(f.item_id == key.0 && f.category_id == key.1));
                    self.status_message = Some("Removed from favorites".to_string());
                } else {
                    self.favorites.push(Favorite {
                        item_id,
                        category_id,
                        name,
                        group,
                    });
                    self.status_message = Some("Added to favorites".to_string());
                }
                self.persist();
                Task::none()
            }
            M::OpenFavorite(item_id, category_id, name) => {
                // Synthesize an AsmEntry from the persisted fields and load
                // its files. Year/rating/updated aren't persisted — that's
                // fine for the detail view.
                let entry = AsmEntry {
                    item_id: item_id.clone(),
                    category_id,
                    name,
                    group: None,
                    handle: None,
                    year: None,
                    rating: None,
                    site_rating: None,
                    updated: None,
                    released: None,
                    event: None,
                    place: None,
                };
                self.selected_entry = Some(entry);
                self.entry_files.clear();
                self.selected_file_id = None;
                self.view_state = ViewState::EntryDetails;
                self.is_loading = true;
                self.status_message = Some("Loading entry…".to_string());
                self.screenshot_handle = None;
                self.screenshot_loading = true;
                let load_files = self.spawn_load_files(item_id.clone(), category_id);
                let load_shot = self.spawn_screenshot(item_id, category_id);
                Task::batch([load_files, load_shot])
            }

            // ── external link ───────────────────────────────────────────
            M::OpenInBrowser(url) => {
                if let Err(e) = open::that_detached(&url) {
                    self.status_message = Some(format!("Open failed: {}", e));
                }
                Task::none()
            }

            // ── entry selection ─────────────────────────────────────────
            M::SelectEntry(item_id, category_id) => {
                let entry = self
                    .results
                    .iter()
                    .find(|e| e.item_id == item_id && e.category_id == category_id)
                    .cloned();
                self.selected_entry = entry;
                self.entry_files.clear();
                self.selected_file_id = None;
                self.view_state = ViewState::EntryDetails;
                self.is_loading = true;
                self.status_message = Some("Loading entry…".to_string());
                self.screenshot_handle = None;
                self.screenshot_loading = true;
                let load_files = self.spawn_load_files(item_id.clone(), category_id);
                let load_shot = self.spawn_screenshot(item_id, category_id);
                Task::batch([load_files, load_shot])
            }
            M::EntryFilesLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(batch) => {
                        // Drop stale responses if user already navigated away.
                        if let Some(entry) = &self.selected_entry {
                            if entry.item_id != batch.item_id
                                || entry.category_id != batch.category_id
                            {
                                return Task::none();
                            }
                        }
                        let count = batch.files.len();
                        self.entry_files = batch.files;
                        self.status_message = Some(format!("{} file(s)", count));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to load files: {}", e));
                    }
                }
                Task::none()
            }
            M::BackToList => {
                self.selected_entry = None;
                self.entry_files.clear();
                self.selected_file_id = None;
                self.extracted_zip = None;
                self.selected_extracted_file_index = None;
                self.screenshot_handle = None;
                self.screenshot_loading = false;
                self.view_state = if self.results.is_empty() {
                    ViewState::Favorites
                } else {
                    ViewState::Results
                };
                Task::none()
            }

            // ── screenshot ──────────────────────────────────────────────
            M::ScreenshotLoaded(result) => {
                self.screenshot_loading = false;
                self.screenshot_handle = match result {
                    Ok(Some(bytes)) => Some(iced::widget::image::Handle::from_bytes(bytes)),
                    _ => None,
                };
                Task::none()
            }

            // ── files: select / download / run / mount ─────────────────
            M::SelectFile(id) => {
                self.selected_file_id = Some(id);
                Task::none()
            }
            M::DownloadFile(file_id) => {
                let Some(entry) = self.selected_entry.clone() else {
                    return Task::none();
                };
                let Some(file) = self
                    .entry_files
                    .iter()
                    .find(|f| f.file_id == file_id)
                    .cloned()
                else {
                    return Task::none();
                };
                self.is_loading = true;
                self.status_message = Some(format!("Downloading {}…", file.path));
                let user_agent = self.user_agent.clone();
                let out_dir = self.download_dir.join(sanitize_dirname(
                    &self.category_registry.label(entry.category_id),
                ));
                Task::perform(
                    async move {
                        let client = Assembly64Client::with_defaults(&user_agent)
                            .map_err(|e| e.to_string())?;
                        let bytes = tokio::time::timeout(
                            tokio::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS),
                            client.download(&entry.item_id, entry.category_id, file.file_id),
                        )
                        .await
                        .map_err(|_| "Download timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                        tokio::fs::create_dir_all(&out_dir)
                            .await
                            .map_err(|e| e.to_string())?;
                        let safe_name = sanitize_filename(&file.path);
                        let out_path = out_dir.join(&safe_name);
                        tokio::fs::write(&out_path, &bytes)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(out_path)
                    },
                    M::DownloadCompleted,
                )
            }
            M::DownloadCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(path) => {
                        self.status_message = Some(format!("Saved to {}", path.display()));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Download failed: {}", e));
                    }
                }
                Task::none()
            }

            M::RunFile(file_id) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".into());
                    return Task::none();
                }
                Task::done(M::CheckDriveBeforeAction(AsmPendingAction::RunFile(
                    file_id,
                )))
            }
            M::DoRunFile(file_id) => self.run_or_mount_file(file_id, None, connection),

            M::RunFileCompleted(result) => {
                self.is_loading = false;
                self.status_message = Some(match result {
                    Ok(s) => s,
                    Err(e) => format!("Run failed: {}", e),
                });
                Task::none()
            }

            M::MountFile(file_id, mode) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".into());
                    return Task::none();
                }
                Task::done(M::CheckDriveBeforeAction(AsmPendingAction::MountFile(
                    file_id, mode,
                )))
            }
            M::DoMountFile(file_id, mode) => {
                self.run_or_mount_file(file_id, Some(mode), connection)
            }

            M::MountCompleted(result) => {
                self.is_loading = false;
                self.status_message = Some(match result {
                    Ok(s) => s,
                    Err(e) => format!("Mount failed: {}", e),
                });
                Task::none()
            }

            // ── drive enable dialog ─────────────────────────────────────
            M::CheckDriveBeforeAction(action) => {
                let drive = self.selected_drive.to_drive_string();
                let host = host.filter(|h| !h.is_empty());
                if let Some(h) = host {
                    self.is_loading = true;
                    self.status_message = Some("Checking drive…".into());
                    Task::perform(
                        crate::file_browser::check_drive_enabled_async(h, drive, password),
                        move |r| M::DriveCheckComplete(r, action.clone()),
                    )
                } else {
                    self.dispatch_action(action)
                }
            }
            M::DriveCheckComplete(result, action) => {
                self.is_loading = false;
                match result {
                    Ok(true) => {
                        self.status_message = None;
                        self.dispatch_action(action)
                    }
                    Ok(false) => {
                        self.drive_enable_dialog = Some((self.selected_drive, action));
                        self.status_message = None;
                        Task::none()
                    }
                    Err(_) => self.dispatch_action(action),
                }
            }
            M::ConfirmEnableDrive => {
                if let Some((drive_opt, action)) = self.drive_enable_dialog.take() {
                    let drive = drive_opt.to_drive_string();
                    if let Some(h) = host {
                        self.is_loading = true;
                        self.status_message = Some(format!(
                            "Enabling Drive {} temporarily…",
                            drive.to_uppercase()
                        ));
                        // Re-store so EnableDriveComplete can dispatch.
                        self.drive_enable_dialog = Some((drive_opt, action));
                        Task::perform(
                            crate::file_browser::enable_drive_async(h, drive, password),
                            M::EnableDriveComplete,
                        )
                    } else {
                        Task::none()
                    }
                } else {
                    Task::none()
                }
            }
            M::CancelEnableDrive => {
                self.drive_enable_dialog = None;
                self.status_message = Some("Cancelled".into());
                Task::none()
            }
            M::EnableDriveComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(()) => {
                        self.status_message = Some("Drive enabled (reboot to restore)".into());
                        if let Some((_, action)) = self.drive_enable_dialog.take() {
                            return self.dispatch_action(action);
                        }
                    }
                    Err(e) => {
                        self.drive_enable_dialog = None;
                        self.status_message = Some(format!("Enable failed: {}", e));
                    }
                }
                Task::none()
            }

            M::DriveSelected(d) => {
                self.selected_drive = d;
                Task::none()
            }
            M::FilterChanged(f) => {
                self.file_filter = f;
                Task::none()
            }

            // ── ZIP ─────────────────────────────────────────────────────
            M::ExtractZip(file_id) => {
                let Some(entry) = self.selected_entry.clone() else {
                    return Task::none();
                };
                let Some(file) = self
                    .entry_files
                    .iter()
                    .find(|f| f.file_id == file_id)
                    .cloned()
                else {
                    return Task::none();
                };
                if !crate::file_types::is_zip_file(&file.ext()) {
                    self.status_message = Some("Not a ZIP file".into());
                    return Task::none();
                }
                self.is_loading = true;
                self.status_message = Some(format!("Extracting {}…", file.path));
                let user_agent = self.user_agent.clone();
                let safe_name = sanitize_filename(&file.path);
                let zip_stem = std::path::Path::new(&safe_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(sanitize_dirname)
                    .unwrap_or_else(|| "extracted".to_string());
                let target = self
                    .download_dir
                    .join(sanitize_dirname(
                        &self.category_registry.label(entry.category_id),
                    ))
                    .join(zip_stem);
                Task::perform(
                    async move {
                        let client = Assembly64Client::with_defaults(&user_agent)
                            .map_err(|e| e.to_string())?;
                        let bytes = tokio::time::timeout(
                            tokio::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS),
                            client.download(&entry.item_id, entry.category_id, file.file_id),
                        )
                        .await
                        .map_err(|_| "Download timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                        tokio::task::spawn_blocking(move || {
                            extract_zip_to_dir(&bytes, &safe_name, &target)
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .map_err(|e| format!("Task error: {}", e))?
                    },
                    M::ZipExtracted,
                )
            }
            M::ZipExtracted(result) => {
                self.is_loading = false;
                match result {
                    Ok(extracted) => {
                        let count = extracted.files.len();
                        let runnable = runnable_extracted_files(&extracted.files).len();
                        self.status_message =
                            Some(format!("Extracted {} ({} runnable)", count, runnable));
                        self.extracted_zip = Some(extracted);
                        self.selected_extracted_file_index = None;
                        self.view_state = ViewState::ZipContents;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Extraction failed: {}", e));
                    }
                }
                Task::none()
            }
            M::SelectExtractedFile(i) => {
                self.selected_extracted_file_index = Some(i);
                Task::none()
            }
            M::RunExtractedFile(i) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected".into());
                    return Task::none();
                }
                Task::done(M::CheckDriveBeforeAction(
                    AsmPendingAction::RunExtractedFile(i),
                ))
            }
            M::DoRunExtractedFile(i) => self.run_or_mount_extracted(i, None, connection),
            M::RunExtractedFileCompleted(r) => {
                self.is_loading = false;
                self.status_message = Some(match r {
                    Ok(s) => s,
                    Err(e) => format!("Run failed: {}", e),
                });
                Task::none()
            }
            M::MountExtractedFile(i, mode) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected".into());
                    return Task::none();
                }
                Task::done(M::CheckDriveBeforeAction(
                    AsmPendingAction::MountExtractedFile(i, mode),
                ))
            }
            M::DoMountExtractedFile(i, mode) => {
                self.run_or_mount_extracted(i, Some(mode), connection)
            }
            M::MountExtractedFileCompleted(r) => {
                self.is_loading = false;
                self.status_message = Some(match r {
                    Ok(s) => s,
                    Err(e) => format!("Mount failed: {}", e),
                });
                Task::none()
            }
            M::CloseZipView => {
                self.extracted_zip = None;
                self.selected_extracted_file_index = None;
                self.view_state = ViewState::EntryDetails;
                Task::none()
            }
        }
    }

    // -------------------------------------------------------------------------
    // Spawn helpers
    // -------------------------------------------------------------------------

    fn spawn_search(&self, query: String, offset: u32) -> Task<Assembly64BrowserMessage> {
        let user_agent = self.user_agent.clone();
        Task::perform(
            async move {
                let client =
                    Assembly64Client::with_defaults(&user_agent).map_err(|e| e.to_string())?;
                let result = tokio::time::timeout(
                    tokio::time::Duration::from_secs(HTTP_TIMEOUT_SECS),
                    client.search(&query, offset, DEFAULT_PAGE_SIZE),
                )
                .await
                .map_err(|_| "Search timed out".to_string())?;
                match result {
                    Ok(entries) => Ok(SearchResultsBatch {
                        query,
                        offset,
                        entries,
                    }),
                    Err(AssemblyError::AqlSyntax) => {
                        Err("AQL syntax error — check your query".to_string())
                    }
                    Err(e) => Err(e.to_string()),
                }
            },
            Assembly64BrowserMessage::SearchCompleted,
        )
    }

    fn spawn_load_files(
        &self,
        item_id: String,
        category_id: u16,
    ) -> Task<Assembly64BrowserMessage> {
        let user_agent = self.user_agent.clone();
        let id_for_response = item_id.clone();
        Task::perform(
            async move {
                let client =
                    Assembly64Client::with_defaults(&user_agent).map_err(|e| e.to_string())?;
                let files = tokio::time::timeout(
                    tokio::time::Duration::from_secs(HTTP_TIMEOUT_SECS),
                    client.list_files(&item_id, category_id),
                )
                .await
                .map_err(|_| "Listing timed out".to_string())?
                .map_err(|e| e.to_string())?;
                Ok(EntryFilesBatch {
                    item_id: id_for_response,
                    category_id,
                    files,
                })
            },
            Assembly64BrowserMessage::EntryFilesLoaded,
        )
    }

    fn spawn_screenshot(
        &self,
        item_id: String,
        category_id: u16,
    ) -> Task<Assembly64BrowserMessage> {
        let user_agent = self.user_agent.clone();
        Task::perform(
            async move {
                let client = crate::net_utils::build_external_client(&user_agent, 15)
                    .map_err(|e| e.to_string())?;
                crate::csdb_screenshots::fetch_screenshot(&client, &item_id, category_id)
                    .await
                    .map_err(|e| e.to_string())
            },
            Assembly64BrowserMessage::ScreenshotLoaded,
        )
    }

    fn dispatch_action(&self, action: AsmPendingAction) -> Task<Assembly64BrowserMessage> {
        use Assembly64BrowserMessage as M;
        match action {
            AsmPendingAction::RunFile(id) => Task::done(M::DoRunFile(id)),
            AsmPendingAction::MountFile(id, mode) => Task::done(M::DoMountFile(id, mode)),
            AsmPendingAction::RunExtractedFile(i) => Task::done(M::DoRunExtractedFile(i)),
            AsmPendingAction::MountExtractedFile(i, mode) => {
                Task::done(M::DoMountExtractedFile(i, mode))
            }
        }
    }

    /// Run-or-mount path used for both Run buttons (mount=None) and Mount
    /// buttons (mount=Some(mode)). Downloads the file, then on the device:
    /// - PRG → run_prg, CRT → run_crt, SID → sid_play
    /// - Disk image → write to temp, mount, optional reset+autoload
    fn run_or_mount_file(
        &mut self,
        file_id: u64,
        mount: Option<MountMode>,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<Assembly64BrowserMessage> {
        let Some(entry) = self.selected_entry.clone() else {
            return Task::none();
        };
        let Some(file) = self
            .entry_files
            .iter()
            .find(|f| f.file_id == file_id)
            .cloned()
        else {
            return Task::none();
        };
        let Some(conn) = connection else {
            self.status_message = Some("Not connected".into());
            return Task::none();
        };
        let user_agent = self.user_agent.clone();
        let drive = self.selected_drive.to_drive_string();
        let device_num = self.selected_drive.device_number().to_string();
        self.is_loading = true;
        let action = if mount.is_some() {
            "Mounting"
        } else {
            "Running"
        };
        self.status_message = Some(format!("{} {}…", action, file.path));
        let result_msg: fn(Result<String, String>) -> Assembly64BrowserMessage = if mount.is_some()
        {
            Assembly64BrowserMessage::MountCompleted
        } else {
            Assembly64BrowserMessage::RunFileCompleted
        };
        Task::perform(
            async move {
                let client =
                    Assembly64Client::with_defaults(&user_agent).map_err(|e| e.to_string())?;
                let bytes = tokio::time::timeout(
                    tokio::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS),
                    client.download(&entry.item_id, entry.category_id, file.file_id),
                )
                .await
                .map_err(|_| "Download timed out".to_string())?
                .map_err(|e| e.to_string())?;

                let ext = file.ext();
                let safe_name = sanitize_filename(&file.path);

                tokio::time::timeout(
                    tokio::time::Duration::from_secs(60),
                    tokio::task::spawn_blocking(move || -> Result<String, String> {
                        let conn = conn.blocking_lock();
                        match (mount, ext.as_str()) {
                            (Some(mode), "d64" | "d71" | "d81" | "g64") => {
                                let temp = std::env::temp_dir().join(&safe_name);
                                std::fs::write(&temp, &bytes)
                                    .map_err(|e| format!("temp write: {}", e))?;
                                let mode = match mode {
                                    MountMode::ReadOnly => ultimate64::drives::MountMode::ReadOnly,
                                    MountMode::ReadWrite => {
                                        ultimate64::drives::MountMode::ReadWrite
                                    }
                                };
                                conn.mount_disk_image(&temp, drive, mode, false)
                                    .map_err(|e| format!("mount: {}", e))?;
                                Ok(format!("Mounted: {}", safe_name))
                            }
                            (Some(_), _) => Err("Only disk images can be mounted".into()),
                            (None, "prg") => conn
                                .run_prg(&bytes)
                                .map(|_| format!("Running: {}", safe_name))
                                .map_err(|e| e.to_string()),
                            (None, "crt") => conn
                                .run_crt(&bytes)
                                .map(|_| format!("Running cartridge: {}", safe_name))
                                .map_err(|e| e.to_string()),
                            (None, "sid") => conn
                                .sid_play(&bytes, None)
                                .map(|_| format!("Playing: {}", safe_name))
                                .map_err(|e| e.to_string()),
                            (None, "d64" | "d71" | "d81" | "g64") => {
                                let temp = std::env::temp_dir().join(&safe_name);
                                std::fs::write(&temp, &bytes)
                                    .map_err(|e| format!("temp write: {}", e))?;
                                conn.mount_disk_image(
                                    &temp,
                                    drive,
                                    ultimate64::drives::MountMode::ReadOnly,
                                    false,
                                )
                                .map_err(|e| format!("mount: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                conn.reset().map_err(|e| format!("reset: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_secs(3));
                                conn.type_text(&format!("load \"*\",{},1\n", device_num))
                                    .map_err(|e| format!("type: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_secs(5));
                                conn.type_text("run\n")
                                    .map_err(|e| format!("type: {}", e))?;
                                Ok(format!("Running disk: {}", safe_name))
                            }
                            (None, other) => Err(format!("Unsupported file type: {}", other)),
                        }
                    }),
                )
                .await
                .map_err(|_| "Operation timed out".to_string())?
                .map_err(|e| format!("Task error: {}", e))?
            },
            result_msg,
        )
    }

    /// Same shape as `run_or_mount_file` but for files already extracted to disk.
    fn run_or_mount_extracted(
        &mut self,
        index: usize,
        mount: Option<MountMode>,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<Assembly64BrowserMessage> {
        let Some(extracted) = &self.extracted_zip else {
            return Task::none();
        };
        let Some(file) = extracted.files.iter().find(|f| f.index == index).cloned() else {
            return Task::none();
        };
        let Some(conn) = connection else {
            self.status_message = Some("Not connected".into());
            return Task::none();
        };
        self.is_loading = true;
        let action = if mount.is_some() {
            "Mounting"
        } else {
            "Running"
        };
        self.status_message = Some(format!("{} {}…", action, file.filename));
        let drive = self.selected_drive.to_drive_string();
        let device_num = self.selected_drive.device_number().to_string();
        let result_msg: fn(Result<String, String>) -> Assembly64BrowserMessage = if mount.is_some()
        {
            Assembly64BrowserMessage::MountExtractedFileCompleted
        } else {
            Assembly64BrowserMessage::RunExtractedFileCompleted
        };
        Task::perform(
            async move {
                let path = file.path.clone();
                let filename = file.filename.clone();
                let ext = file.ext.clone();
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(60),
                    tokio::task::spawn_blocking(move || -> Result<String, String> {
                        let conn = conn.blocking_lock();
                        let data = std::fs::read(&path).map_err(|e| format!("read: {}", e))?;
                        match (mount, ext.as_str()) {
                            (Some(mode), "d64" | "d71" | "d81" | "g64") => {
                                let mode = match mode {
                                    MountMode::ReadOnly => ultimate64::drives::MountMode::ReadOnly,
                                    MountMode::ReadWrite => {
                                        ultimate64::drives::MountMode::ReadWrite
                                    }
                                };
                                conn.mount_disk_image(&path, drive, mode, false)
                                    .map_err(|e| format!("mount: {}", e))?;
                                Ok(format!("Mounted: {}", filename))
                            }
                            (Some(_), _) => Err("Only disk images can be mounted".into()),
                            (None, "prg") => conn
                                .run_prg(&data)
                                .map(|_| format!("Running: {}", filename))
                                .map_err(|e| e.to_string()),
                            (None, "crt") => conn
                                .run_crt(&data)
                                .map(|_| format!("Running cartridge: {}", filename))
                                .map_err(|e| e.to_string()),
                            (None, "sid") => conn
                                .sid_play(&data, None)
                                .map(|_| format!("Playing: {}", filename))
                                .map_err(|e| e.to_string()),
                            (None, "d64" | "d71" | "d81" | "g64") => {
                                conn.mount_disk_image(
                                    &path,
                                    drive,
                                    ultimate64::drives::MountMode::ReadOnly,
                                    false,
                                )
                                .map_err(|e| format!("mount: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_millis(500));
                                conn.reset().map_err(|e| format!("reset: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_secs(3));
                                conn.type_text(&format!("load \"*\",{},1\n", device_num))
                                    .map_err(|e| format!("type: {}", e))?;
                                std::thread::sleep(std::time::Duration::from_secs(5));
                                conn.type_text("run\n")
                                    .map_err(|e| format!("type: {}", e))?;
                                Ok(format!("Running disk: {}", filename))
                            }
                            (None, other) => Err(format!("Unsupported file type: {}", other)),
                        }
                    }),
                )
                .await
                .map_err(|_| "Operation timed out".to_string())?
                .map_err(|e| format!("Task error: {}", e))?
            },
            result_msg,
        )
    }

    // -------------------------------------------------------------------------
    // view
    // -------------------------------------------------------------------------

    pub fn view(
        &self,
        font_size: u32,
        is_connected: bool,
    ) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let search_bar = self.view_search_bar(font_size);

        let content: Element<'_, Assembly64BrowserMessage> = match self.view_state {
            ViewState::Results => self.view_results(font_size),
            ViewState::EntryDetails => self.view_entry_details(font_size, is_connected),
            ViewState::ZipContents => self.view_zip_contents(font_size, is_connected),
            ViewState::Favorites => self.view_favorites(font_size),
        };

        let status = if self.is_loading {
            text(self.status_message.as_deref().unwrap_or("Loading…")).size(fs.small)
        } else if let Some(msg) = &self.status_message {
            text(msg).size(fs.small)
        } else {
            text("Ready").size(fs.small)
        };

        let connection_status = if is_connected {
            text("● Connected")
                .size(fs.small)
                .color(iced::Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            text("○ Not connected")
                .size(fs.small)
                .color(iced::Color::from_rgb(0.8, 0.5, 0.2))
        };

        let status_bar = row![status, Space::new().width(Length::Fill), connection_status]
            .spacing(10)
            .align_y(iced::Alignment::Center);

        if let Some((drive_opt, _)) = &self.drive_enable_dialog {
            let drive_letter = drive_opt.to_drive_string().to_uppercase();
            let drive_num = drive_opt.device_number();
            let dialog = container(
                column![
                    text(format!(
                        "Drive {} (device {}) is currently disabled.",
                        drive_letter, drive_num
                    ))
                    .size(fs.normal),
                    text("Enable temporarily? (reboot restores original settings)").size(fs.small),
                    row![
                        button(text(format!("Enable Drive {}", drive_letter)).size(fs.small))
                            .on_press(Assembly64BrowserMessage::ConfirmEnableDrive)
                            .padding([5, 15]),
                        button(text("Cancel").size(fs.small))
                            .on_press(Assembly64BrowserMessage::CancelEnableDrive)
                            .padding([5, 15]),
                    ]
                    .spacing(10),
                ]
                .spacing(12)
                .padding(20),
            )
            .style(container::bordered_box)
            .width(Length::Fill);
            return column![
                search_bar,
                rule::horizontal(1),
                dialog,
                rule::horizontal(1),
                status_bar
            ]
            .spacing(5)
            .padding(5)
            .into();
        }

        column![
            search_bar,
            rule::horizontal(1),
            content,
            rule::horizontal(1),
            status_bar
        ]
        .spacing(5)
        .padding(5)
        .into()
    }

    fn view_search_bar(&self, font_size: u32) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let primary_row = row![
            text_input("Search Assembly64…", &self.search_form.free_text)
                .on_input(Assembly64BrowserMessage::FreeTextChanged)
                .on_submit(Assembly64BrowserMessage::SearchSubmit)
                .padding(8)
                .size(fs.normal)
                .width(Length::FillPortion(3)),
            pick_list(
                self.presets.types.clone(),
                Some(self.search_form.type_filter.clone()),
                Assembly64BrowserMessage::TypeFilterChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(140.0)),
            pick_list(
                self.presets.sources.clone(),
                Some(self.search_form.source_filter.clone()),
                Assembly64BrowserMessage::SourceFilterChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(160.0)),
            pick_list(
                RatingFilter::ALL.to_vec(),
                Some(self.search_form.rating_filter),
                Assembly64BrowserMessage::RatingFilterChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(110.0)),
            pick_list(
                RecencyFilter::ALL.to_vec(),
                Some(self.search_form.recency_filter),
                Assembly64BrowserMessage::RecencyFilterChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(110.0)),
            pick_list(
                SortOrder::ALL.to_vec(),
                Some(self.search_form.sort_order),
                Assembly64BrowserMessage::SortChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(140.0)),
            tooltip(
                button(text("Search").size(fs.normal))
                    .on_press(Assembly64BrowserMessage::SearchSubmit)
                    .padding([8, 12]),
                "Run search",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Latest").size(fs.normal))
                    .on_press(Assembly64BrowserMessage::ResetAndShowLatest)
                    .padding([8, 12]),
                "Clear filters and show the most recent entries",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("★").size(fs.normal))
                    .on_press(Assembly64BrowserMessage::ShowFavorites)
                    .padding([8, 10]),
                "Favorites",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let saved_row = self.view_saved_searches_row(font_size);

        let advanced_toggle = row![checkbox(self.show_advanced_query)
            .on_toggle(Assembly64BrowserMessage::ToggleAdvancedQuery)
            .label("Advanced query")
            .text_size(fs.tiny),]
        .padding([0, 4]);

        let advanced_block: Element<'_, Assembly64BrowserMessage> = if self.show_advanced_query {
            let composed = self.search_form.compose_aql();
            container(
                column![
                    text(format!("AQL: {}", if composed.is_empty() { "(empty)" } else { &composed }))
                        .size(fs.tiny)
                        .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
                    text("Tip: type a `:` in the search box to enter raw AQL (e.g. `group:fairlight`).")
                        .size(fs.tiny)
                        .color(iced::Color::from_rgb(0.45, 0.45, 0.5)),
                ]
                .spacing(2)
                .padding([4, 8]),
            )
            .into()
        } else {
            Space::new().height(0).into()
        };

        column![primary_row, saved_row, advanced_toggle, advanced_block]
            .spacing(4)
            .into()
    }

    fn view_saved_searches_row(&self, font_size: u32) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let mut chips: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        chips.push(text("Saved:").size(fs.tiny).into());

        if self.saved_searches.is_empty() {
            chips.push(
                text("(none)")
                    .size(fs.tiny)
                    .color(iced::Color::from_rgb(0.5, 0.5, 0.6))
                    .into(),
            );
        } else {
            for s in &self.saved_searches {
                let name = s.name.clone();
                let remove_name = s.name.clone();
                chips.push(
                    button(text(&s.name).size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::ApplySavedSearch(name))
                        .padding([2, 6])
                        .into(),
                );
                chips.push(
                    button(text("×").size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::RemoveSavedSearch(remove_name))
                        .padding([2, 4])
                        .style(button::text)
                        .into(),
                );
            }
        }

        chips.push(Space::new().width(15).into());
        chips.push(
            text_input("Name…", &self.new_saved_search_name)
                .on_input(Assembly64BrowserMessage::NewSavedSearchNameChanged)
                .on_submit(Assembly64BrowserMessage::SaveCurrentSearch)
                .padding(4)
                .size(fs.tiny)
                .width(Length::Fixed(120.0))
                .into(),
        );
        chips.push(
            button(text("Save current").size(fs.tiny))
                .on_press(Assembly64BrowserMessage::SaveCurrentSearch)
                .padding([2, 8])
                .into(),
        );

        row(chips)
            .spacing(4)
            .align_y(iced::Alignment::Center)
            .into()
    }

    fn view_results(&self, font_size: u32) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        if self.results.is_empty() {
            let placeholder = if self.is_loading {
                "Loading…"
            } else if !self.last_query.is_empty() {
                "No results — try different filters."
            } else {
                "Type a search and pick filters, or click Search to load the latest entries."
            };
            return container(text(placeholder).size(fs.normal))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20)
                .into();
        }

        let header = row![
            text("Results").size(fs.large),
            Space::new().width(Length::Fill),
            text(format!(
                "{} {}{}",
                self.results.len(),
                if self.results.len() == 1 {
                    "entry"
                } else {
                    "entries"
                },
                if self.has_more { " (+ more)" } else { "" }
            ))
            .size(fs.small),
        ]
        .align_y(iced::Alignment::Center);

        let col_header = row![
            text("Name")
                .size(fs.tiny)
                .width(Length::Fixed(280.0))
                .color(muted_color()),
            text("Group")
                .size(fs.tiny)
                .width(Length::Fixed(160.0))
                .color(muted_color()),
            text("Year")
                .size(fs.tiny)
                .width(Length::Fixed(50.0))
                .color(muted_color()),
            text("Rating")
                .size(fs.tiny)
                .width(Length::Fixed(140.0))
                .color(muted_color()),
            text("Source")
                .size(fs.tiny)
                .width(Length::Fixed(150.0))
                .color(muted_color()),
            text("Added")
                .size(fs.tiny)
                .width(Length::Fill)
                .color(muted_color()),
            Space::new().width(Length::Fixed(110.0)),
        ]
        .spacing(5)
        .padding([2, 0]);

        let mut items: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        for entry in &self.results {
            items.push(self.view_result_row(entry, font_size));
            items.push(rule::horizontal(1).into());
        }
        if self.has_more {
            items.push(
                container(
                    button(
                        text(if self.is_loading {
                            "Loading…"
                        } else {
                            "Load more"
                        })
                        .size(fs.normal),
                    )
                    .on_press(Assembly64BrowserMessage::LoadMore)
                    .padding([6, 18]),
                )
                .width(Length::Fill)
                .center_x(Length::Fill)
                .padding(8)
                .into(),
            );
        }

        let list = scrollable(
            Column::with_children(items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![
            header,
            rule::horizontal(1),
            col_header,
            rule::horizontal(1),
            list
        ]
        .spacing(5)
        .into()
    }

    fn view_result_row<'a>(
        &'a self,
        entry: &'a AsmEntry,
        font_size: u32,
    ) -> Element<'a, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let is_fav = self
            .favorites
            .iter()
            .any(|f| f.item_id == entry.item_id && f.category_id == entry.category_id);
        let star_icon = if is_fav { "★" } else { "☆" };

        let name_display = truncate(&entry.name, NAME_DISPLAY_CAP);
        let group_display = entry
            .group
            .as_deref()
            .map(|g| truncate(g, GROUP_DISPLAY_CAP))
            .unwrap_or_default();
        let year_display = entry.year.map(|y| y.to_string()).unwrap_or_default();
        let stars = rating_stars(entry.rating);
        let source = self.category_registry.label(entry.category_id);
        let updated = entry.updated.clone().unwrap_or_default();

        let item_id = entry.item_id.clone();
        let cat_id = entry.category_id;

        let mut row_widget = row![
            tooltip(
                button(text(name_display).size(fs.normal))
                    .on_press(Assembly64BrowserMessage::SelectEntry(
                        item_id.clone(),
                        cat_id,
                    ))
                    .padding([4, 8])
                    .width(Length::Fixed(280.0))
                    .style(button::text),
                text(&entry.name).size(fs.normal),
                tooltip::Position::Top,
            )
            .style(container::bordered_box),
            text(group_display)
                .size(fs.tiny)
                .width(Length::Fixed(160.0))
                .color(iced::Color::from_rgb(0.5, 0.7, 0.9)),
            text(year_display)
                .size(fs.tiny)
                .width(Length::Fixed(50.0))
                .color(iced::Color::from_rgb(0.7, 0.7, 0.5)),
            text(stars)
                .size(fs.tiny)
                .width(Length::Fixed(140.0))
                .color(iced::Color::from_rgb(0.85, 0.7, 0.2)),
            text(source)
                .size(fs.tiny)
                .width(Length::Fixed(150.0))
                .color(source_color(entry.category_id)),
            text(updated)
                .size(fs.tiny)
                .width(Length::Fill)
                .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
            tooltip(
                button(text(star_icon).size(fs.small))
                    .on_press(Assembly64BrowserMessage::ToggleFavorite(
                        item_id.clone(),
                        cat_id,
                        entry.name.clone(),
                        entry.group.clone(),
                    ))
                    .padding([4, 8])
                    .style(button::text),
                if is_fav {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                },
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Open").size(fs.small))
                    .on_press(Assembly64BrowserMessage::SelectEntry(item_id, cat_id))
                    .padding([4, 10]),
                "Show files for this entry",
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center)
        .padding([4, 0]);

        // Optional CSDB scene-comments link for CSDB-derived entries.
        if let Some(url) = entry.csdb_release_url() {
            row_widget = row_widget.push(
                tooltip(
                    button(text("CSDB").size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::OpenInBrowser(url))
                        .padding([4, 6])
                        .style(button::text),
                    "Open release page on csdb.dk",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        row_widget.into()
    }

    fn view_favorites(&self, font_size: u32) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        if self.favorites.is_empty() {
            return container(
                column![
                    text("Favorites").size(fs.large),
                    Space::new().height(20),
                    text("No favorites yet — click the ☆ next to a result to add one.")
                        .size(fs.normal),
                ]
                .spacing(8)
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .padding(20)
            .into();
        }

        let header = row![
            text(format!("Favorites ({})", self.favorites.len())).size(fs.large),
            Space::new().width(Length::Fill),
        ]
        .align_y(iced::Alignment::Center);

        let mut items: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        for fav in &self.favorites {
            let item_id = fav.item_id.clone();
            let cat_id = fav.category_id;
            let name = fav.name.clone();
            let group = fav.group.clone();

            let group_display = group.as_deref().unwrap_or("");
            let label = self.category_registry.label(cat_id);

            let row_w = row![
                tooltip(
                    button(text(truncate(&fav.name, NAME_DISPLAY_CAP)).size(fs.normal))
                        .on_press(Assembly64BrowserMessage::OpenFavorite(
                            item_id.clone(),
                            cat_id,
                            name.clone(),
                        ))
                        .padding([4, 8])
                        .width(Length::Fixed(320.0))
                        .style(button::text),
                    text(&fav.name).size(fs.normal),
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text(group_display.to_string())
                    .size(fs.tiny)
                    .width(Length::Fixed(180.0))
                    .color(iced::Color::from_rgb(0.5, 0.7, 0.9)),
                text(label.to_string())
                    .size(fs.tiny)
                    .width(Length::Fixed(160.0))
                    .color(source_color(cat_id)),
                Space::new().width(Length::Fill),
                tooltip(
                    button(text("Open").size(fs.small))
                        .on_press(Assembly64BrowserMessage::OpenFavorite(
                            item_id.clone(),
                            cat_id,
                            name,
                        ))
                        .padding([4, 10]),
                    "Open entry",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("✕").size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::ToggleFavorite(
                            item_id,
                            cat_id,
                            fav.name.clone(),
                            fav.group.clone(),
                        ))
                        .padding([4, 6])
                        .style(button::text),
                    "Remove from favorites",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center)
            .padding([4, 0]);
            items.push(row_w.into());
            items.push(rule::horizontal(1).into());
        }

        let list = scrollable(
            Column::with_children(items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![header, rule::horizontal(1), list].spacing(5).into()
    }

    fn view_entry_details(
        &self,
        font_size: u32,
        is_connected: bool,
    ) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let entry = match &self.selected_entry {
            Some(e) => e,
            None => {
                return container(text("No entry selected").size(fs.normal))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into();
            }
        };

        let header = row![
            tooltip(
                button(text("← Back").size(fs.normal))
                    .on_press(Assembly64BrowserMessage::BackToList)
                    .padding([6, 12]),
                "Back to list",
                tooltip::Position::Right,
            )
            .style(container::bordered_box),
            Space::new().width(10),
            text(&entry.name).size(fs.large),
            Space::new().width(Length::Fill),
            text(format!(
                "{} · ID {}",
                self.category_registry.label(entry.category_id),
                entry.item_id
            ))
            .size(fs.small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let mut info: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        if let Some(g) = &entry.group {
            info.push(text(format!("Group: {}", g)).size(fs.small).into());
        }
        if let Some(y) = entry.year {
            info.push(text(format!("Year: {}", y)).size(fs.small).into());
        }
        if let Some(r) = entry.rating {
            info.push(
                text(format!("Rating: {} {}", r, rating_stars(Some(r))))
                    .size(fs.small)
                    .into(),
            );
        }
        if let Some(u) = &entry.updated {
            info.push(text(format!("Added: {}", u)).size(fs.small).into());
        }
        if let Some(url) = entry.csdb_release_url() {
            info.push(
                button(text("CSDB →").size(fs.tiny))
                    .on_press(Assembly64BrowserMessage::OpenInBrowser(url))
                    .padding([4, 8])
                    .into(),
            );
        }

        let info_row = row(info).spacing(20);

        let filter_row = row![
            text("Filter:").size(fs.small),
            pick_list(
                FileFilter::all(),
                Some(self.file_filter),
                Assembly64BrowserMessage::FilterChanged,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(130.0)),
            Space::new().width(20),
            text("Mount to:").size(fs.small),
            pick_list(
                DriveOption::all(),
                Some(self.selected_drive),
                Assembly64BrowserMessage::DriveSelected,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(110.0)),
            Space::new().width(Length::Fill),
            text(format!("{} file(s)", self.entry_files.len())).size(fs.small),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let filtered: Vec<&AsmFile> = self
            .entry_files
            .iter()
            .filter(|f| self.file_filter.matches(&f.ext()))
            .collect();

        let mut file_items: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        if filtered.is_empty() {
            file_items.push(
                container(text("No files match the current filter").size(fs.normal))
                    .padding(20)
                    .into(),
            );
        } else {
            for (idx, file) in filtered.iter().enumerate() {
                file_items.push(self.view_remote_file_row(idx + 1, file, is_connected, font_size));
                file_items.push(rule::horizontal(1).into());
            }
        }

        let file_list = scrollable(
            Column::with_children(file_items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        let content_area: Element<'_, Assembly64BrowserMessage> =
            if let Some(handle) = &self.screenshot_handle {
                let screenshot = container(
                    iced::widget::image(handle.clone())
                        .width(Length::Fixed(384.0))
                        .height(Length::Fixed(272.0))
                        .content_fit(iced::ContentFit::Contain),
                )
                .width(Length::Fixed(390.0))
                .padding(3)
                .style(container::bordered_box);

                row![
                    column![filter_row, rule::horizontal(1), file_list]
                        .spacing(5)
                        .width(Length::Fill)
                        .height(Length::Fill),
                    Space::new().width(5),
                    screenshot,
                ]
                .height(Length::Fill)
                .into()
            } else if self.screenshot_loading {
                let placeholder = container(
                    column![
                        text("🖼").size(fs.icon),
                        text("Loading preview…").size(fs.small),
                    ]
                    .spacing(8)
                    .align_x(iced::Alignment::Center),
                )
                .width(Length::Fixed(390.0))
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(container::bordered_box);
                row![
                    column![filter_row, rule::horizontal(1), file_list]
                        .spacing(5)
                        .width(Length::Fill)
                        .height(Length::Fill),
                    Space::new().width(5),
                    placeholder,
                ]
                .height(Length::Fill)
                .into()
            } else {
                // No screenshot available (non-CSDB source) — keep the file
                // list full-width rather than showing a broken image slot.
                column![filter_row, rule::horizontal(1), file_list]
                    .spacing(5)
                    .height(Length::Fill)
                    .into()
            };

        column![
            header,
            rule::horizontal(1),
            info_row,
            rule::horizontal(1),
            content_area,
        ]
        .spacing(5)
        .height(Length::Fill)
        .into()
    }

    fn view_remote_file_row<'a>(
        &'a self,
        ordinal: usize,
        file: &'a AsmFile,
        is_connected: bool,
        font_size: u32,
    ) -> Element<'a, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let ext = file.ext();
        let is_selected = self.selected_file_id == Some(file.file_id);
        let is_runnable = crate::file_types::is_runnable(&ext);
        let is_disk_image = crate::file_types::is_disk_image(&ext);
        let is_zip = crate::file_types::is_zip_file(&ext);
        let ext_color = crate::file_types::ext_color(&ext);

        let filename_display = truncate(&file.path, 35);

        let mut row_widget = row![
            text(format!("{:02}.", ordinal))
                .size(fs.tiny)
                .width(Length::Fixed(30.0)),
            tooltip(
                button(text(filename_display).size(fs.normal))
                    .on_press(Assembly64BrowserMessage::SelectFile(file.file_id))
                    .padding([4, 8])
                    .width(Length::Fill)
                    .style(if is_selected {
                        button::primary
                    } else {
                        button::text
                    }),
                text(&file.path).size(fs.normal),
                tooltip::Position::Top,
            )
            .style(container::bordered_box),
            text(ext.to_uppercase())
                .size(fs.tiny)
                .width(Length::Fixed(40.0))
                .color(ext_color),
            text(file.pretty_size())
                .size(fs.tiny)
                .width(Length::Fixed(80.0))
                .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        row_widget = row_widget.push(
            tooltip(
                button(text("↓").size(fs.small))
                    .on_press(Assembly64BrowserMessage::DownloadFile(file.file_id))
                    .padding([4, 8]),
                "Download",
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
        );

        if is_zip {
            row_widget = row_widget.push(
                tooltip(
                    button(text("📦").size(fs.small))
                        .on_press(Assembly64BrowserMessage::ExtractZip(file.file_id))
                        .padding([4, 8]),
                    "Extract ZIP and browse contents",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        if is_disk_image && is_connected {
            let drive_label = self.selected_drive.device_number();
            row_widget = row_widget.push(
                tooltip(
                    button(text(format!("{}:RO", drive_label)).size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::MountFile(
                            file.file_id,
                            MountMode::ReadOnly,
                        ))
                        .padding([4, 6]),
                    text(format!("Mount Drive {} (Read Only)", drive_label)).size(fs.normal),
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
            row_widget = row_widget.push(
                tooltip(
                    button(text(format!("{}:RW", drive_label)).size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::MountFile(
                            file.file_id,
                            MountMode::ReadWrite,
                        ))
                        .padding([4, 6]),
                    text(format!("Mount Drive {} (Read/Write)", drive_label)).size(fs.normal),
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        if is_runnable && is_connected {
            row_widget = row_widget.push(
                tooltip(
                    button(text("▶").size(fs.small))
                        .on_press(Assembly64BrowserMessage::RunFile(file.file_id))
                        .padding([4, 8]),
                    if is_disk_image {
                        "Mount, reset, and run"
                    } else {
                        "Run on Ultimate64"
                    },
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        row_widget.padding([2, 0]).into()
    }

    fn view_zip_contents(
        &self,
        font_size: u32,
        is_connected: bool,
    ) -> Element<'_, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let extracted = match &self.extracted_zip {
            Some(e) => e,
            None => {
                return container(text("No ZIP extracted").size(fs.normal))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into();
            }
        };

        let header = row![
            tooltip(
                button(text("← Back").size(fs.normal))
                    .on_press(Assembly64BrowserMessage::CloseZipView)
                    .padding([6, 12]),
                "Back to entry",
                tooltip::Position::Right,
            )
            .style(container::bordered_box),
            Space::new().width(10),
            text(format!("📦 {}", extracted.source_filename)).size(fs.large),
            Space::new().width(Length::Fill),
            text(format!("{} file(s)", extracted.files.len())).size(fs.small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let drive_row = row![
            text("Mount to:").size(fs.small),
            pick_list(
                DriveOption::all(),
                Some(self.selected_drive),
                Assembly64BrowserMessage::DriveSelected,
            )
            .text_size(fs.normal)
            .width(Length::Fixed(110.0)),
            Space::new().width(Length::Fill),
            text(format!("Extracted to: {}", extracted.extract_dir.display()))
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.5, 0.5, 0.6)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let mut file_items: Vec<Element<'_, Assembly64BrowserMessage>> = Vec::new();
        if extracted.files.is_empty() {
            file_items.push(
                container(text("No files in archive").size(fs.normal))
                    .padding(20)
                    .into(),
            );
        } else {
            for file in &extracted.files {
                file_items.push(self.view_extracted_file_row(file, is_connected, font_size));
                file_items.push(rule::horizontal(1).into());
            }
        }

        let file_list = scrollable(
            Column::with_children(file_items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![
            header,
            rule::horizontal(1),
            drive_row,
            rule::horizontal(1),
            file_list,
        ]
        .spacing(5)
        .into()
    }

    fn view_extracted_file_row<'a>(
        &'a self,
        file: &'a ExtractedFile,
        is_connected: bool,
        font_size: u32,
    ) -> Element<'a, Assembly64BrowserMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);
        let is_selected = self.selected_extracted_file_index == Some(file.index);
        let is_runnable = crate::file_types::is_runnable(&file.ext);
        let is_disk_image = crate::file_types::is_disk_image(&file.ext);
        let ext_color = crate::file_types::ext_color(&file.ext);

        let filename_display = truncate(&file.filename, 40);
        let size_str = if file.size >= 1024 * 1024 {
            format!("{:.1} MB", file.size as f64 / (1024.0 * 1024.0))
        } else if file.size >= 1024 {
            format!("{} KB", file.size / 1024)
        } else {
            format!("{} B", file.size)
        };

        let mut row_widget = row![
            text(format!("{:02}.", file.index))
                .size(fs.tiny)
                .width(Length::Fixed(30.0)),
            tooltip(
                button(text(filename_display).size(fs.normal))
                    .on_press(Assembly64BrowserMessage::SelectExtractedFile(file.index))
                    .padding([4, 8])
                    .width(Length::Fill)
                    .style(if is_selected {
                        button::primary
                    } else {
                        button::text
                    }),
                text(&file.filename).size(fs.normal),
                tooltip::Position::Top,
            )
            .style(container::bordered_box),
            text(file.ext.to_uppercase())
                .size(fs.tiny)
                .width(Length::Fixed(40.0))
                .color(ext_color),
            text(size_str)
                .size(fs.tiny)
                .width(Length::Fixed(80.0))
                .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        if is_disk_image && is_connected {
            let drive_label = self.selected_drive.device_number();
            row_widget = row_widget.push(
                tooltip(
                    button(text(format!("{}:RO", drive_label)).size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::MountExtractedFile(
                            file.index,
                            MountMode::ReadOnly,
                        ))
                        .padding([4, 6]),
                    text(format!("Mount Drive {} (Read Only)", drive_label)).size(fs.normal),
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
            row_widget = row_widget.push(
                tooltip(
                    button(text(format!("{}:RW", drive_label)).size(fs.tiny))
                        .on_press(Assembly64BrowserMessage::MountExtractedFile(
                            file.index,
                            MountMode::ReadWrite,
                        ))
                        .padding([4, 6]),
                    text(format!("Mount Drive {} (Read/Write)", drive_label)).size(fs.normal),
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        if is_runnable && is_connected {
            row_widget = row_widget.push(
                tooltip(
                    button(text("▶").size(fs.small))
                        .on_press(Assembly64BrowserMessage::RunExtractedFile(file.index))
                        .padding([4, 8]),
                    if is_disk_image {
                        "Mount, reset, and run"
                    } else {
                        "Run on Ultimate64"
                    },
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );
        }

        row_widget.padding([2, 0]).into()
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn muted_color() -> iced::Color {
    iced::Color::from_rgb(0.5, 0.5, 0.6)
}

fn source_color(category_id: u16) -> iced::Color {
    if crate::assembly64::is_csdb_category(category_id) {
        iced::Color::from_rgb(0.45, 0.85, 0.55) // CSDB green
    } else if matches!(category_id, 18..=21) {
        iced::Color::from_rgb(0.55, 0.7, 0.95) // HVSC blue
    } else if matches!(category_id, 14 | 15) {
        iced::Color::from_rgb(0.8, 0.65, 0.4) // c64.com amber
    } else if category_id == 33 {
        iced::Color::from_rgb(0.85, 0.55, 0.85) // OneLoad pink
    } else if category_id == 16 {
        iced::Color::from_rgb(0.7, 0.7, 0.95) // Gamebase64
    } else {
        iced::Color::from_rgb(0.7, 0.7, 0.75)
    }
}

fn sanitize_dirname(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() {
        "Other".to_string()
    } else {
        trimmed
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c < ' ' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_long_strings() {
        let s = truncate("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 5);
        assert_eq!(s.chars().count(), 5);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn truncate_passes_short_strings() {
        assert_eq!(truncate("ok", 10), "ok");
    }

    #[test]
    fn sanitize_dirname_replaces_special_chars() {
        assert_eq!(sanitize_dirname("CSDB Demos / Misc"), "CSDB Demos _ Misc");
        assert_eq!(sanitize_dirname(""), "Other");
    }

    #[test]
    fn sanitize_filename_strips_path_separators() {
        assert_eq!(sanitize_filename("a/b/c.d64"), "a_b_c.d64");
    }
}
