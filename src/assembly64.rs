//! Assembly64 REST client — search, list files, download, categories.
//!
//! Wraps the API at <https://hackerswithstyle.se/leet/>.
//! All requests carry the required `client-id: u64manager` header.
//!
//! Behaviors ported from the reference Amiga client at
//! `../u64ctl/src/u64mui/assembly64.c`:
//! - AQL-aware URL encoder (leaves `:` and `*` alone)
//! - Cross-repo dedup by (name, group, year) per page
//! - HTTP 463 → typed `AssemblyError::AqlSyntax`
//! - Category id → human label map covering all known sources

use anyhow::Result;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const BASE_URL: &str = "https://hackerswithstyle.se/leet";
pub const CLIENT_ID: &str = "u64manager";
pub const DEFAULT_PAGE_SIZE: u32 = 100;

#[derive(Debug)]
pub enum AssemblyError {
    /// HTTP 463 — server rejects the AQL string. Surface as inline hint.
    AqlSyntax,
    Http(StatusCode),
    Network(reqwest::Error),
    Json(serde_json::Error),
    Other(String),
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssemblyError::AqlSyntax => f.write_str("AQL syntax error"),
            AssemblyError::Http(s) => write!(f, "HTTP {}", s),
            AssemblyError::Network(e) => write!(f, "{}", e),
            AssemblyError::Json(e) => write!(f, "{}", e),
            AssemblyError::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for AssemblyError {}

impl From<reqwest::Error> for AssemblyError {
    fn from(e: reqwest::Error) -> Self {
        AssemblyError::Network(e)
    }
}

impl From<serde_json::Error> for AssemblyError {
    fn from(e: serde_json::Error) -> Self {
        AssemblyError::Json(e)
    }
}

// -----------------------------------------------------------------------------
// Data models
// -----------------------------------------------------------------------------

/// One search hit. Field types match what the API actually returns; numeric
/// fields are wrapped in Option because the JSON occasionally omits them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsmEntry {
    pub item_id: String,
    pub category_id: u16,
    pub name: String,
    pub group: Option<String>,
    /// Composer/coder/scener names credited on the entry (often comma-separated).
    pub handle: Option<String>,
    pub year: Option<u16>,
    /// User rating 0..=10 from the entry's source repo.
    pub rating: Option<u8>,
    /// Aggregate Assembly64 rating, finer-grained than `rating`.
    pub site_rating: Option<f32>,
    /// Server-side date-added, e.g. "2026-04-01".
    pub updated: Option<String>,
    /// Original release date (may be more precise than `year`).
    pub released: Option<String>,
    /// Demoparty / compo this entry was shown at, when known.
    pub event: Option<String>,
    /// Place in the compo, when applicable.
    pub place: Option<u16>,
    /// Numeric compo-type id — matches `/search/compotypes`.
    pub compo: Option<u16>,
    /// Aggregated Assembly64 category id, distinct from the source-specific
    /// `category_id`. Useful for "show similar" navigation.
    pub site_category: Option<u16>,
}

impl AsmEntry {
    /// True when the entry comes from a CSDB-derived repo, in which case
    /// `item_id` equals the CSDB release id and a screenshot can be
    /// looked up via the CSDB webservice.
    pub fn is_csdb_source(&self) -> bool {
        is_csdb_category(self.category_id)
    }

    /// Open `https://csdb.dk/release/?id=<item_id>` for CSDB-derived items
    /// to read scene comments. Returns None for non-CSDB sources.
    pub fn csdb_release_url(&self) -> Option<String> {
        if self.is_csdb_source() {
            Some(format!("https://csdb.dk/release/?id={}", self.item_id))
        } else {
            None
        }
    }
}

/// One file inside an entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsmFile {
    pub file_id: u64,
    pub path: String,
    pub size_bytes: u64,
}

impl AsmFile {
    pub fn pretty_size(&self) -> String {
        format_size(self.size_bytes)
    }

    pub fn ext(&self) -> String {
        match self.path.rfind('.') {
            Some(pos) => self.path[pos + 1..].to_lowercase(),
            None => String::new(),
        }
    }
}

// -----------------------------------------------------------------------------
// Source / category metadata
// -----------------------------------------------------------------------------

/// Returns true for the CSDB-sourced category ids (0..=10 and 25), where the
/// Assembly64 item_id maps directly to a CSDB release id.
///
/// Why: Assembly64 aggregates many repos and re-uses each repo's native id
/// space, so only entries with CSDB-bound categories can be looked up via
/// the CSDB webservice for screenshots / external links.
pub fn is_csdb_category(category_id: u16) -> bool {
    matches!(category_id, 0..=10 | 25)
}

/// Mapping from u64ctl `assembly64.c:198-236`. Empty string for unknown ids
/// keeps `format!` callers safe.
pub fn category_label(id: u16) -> &'static str {
    match id {
        0 => "CSDB Games",
        1 => "CSDB Demos",
        2 => "CSDB C128",
        3 => "CSDB Graphics",
        4 => "CSDB Music",
        5 => "CSDB Mags",
        6 => "CSDB BBS",
        7 => "CSDB Misc",
        8 => "CSDB Tools",
        9 => "CSDB Charts",
        10 => "CSDB Easyflash",
        11 => "c64.org Intros",
        12 => "c64tapes.org Tapes",
        14 => "c64.com Demos",
        15 => "c64.com Games",
        16 => "Gamebase64",
        17 => "SEUCK",
        18 => "HVSC Music",
        19 => "HVSC Games",
        20 => "HVSC Demos",
        21 => "HVSC Artist",
        22 => "Mayhem CRT",
        23 => "Preservers Disk",
        24 => "Preservers Tape",
        25 => "CSDB REU",
        33 => "OneLoad64 Games",
        35 => "Ultimate Tape Arc.",
        36 => "Commodore Games",
        37 => "Commodore Demos",
        38 => "Commodore Graphics",
        39 => "Commodore Music",
        40 => "Commodore Apps",
        _ => "",
    }
}

/// One option in a Source / Type / Subcategory dropdown.
///
/// `aql_key` is what gets dropped into the AQL query when this choice is
/// selected. An empty key means "Any" / no filter — the dropdown still shows
/// the `label`, but `compose_aql` skips the fragment.
///
/// Populated dynamically from `/search/aql/presets`; falls back to a
/// hardcoded baseline (`Choice::baseline_sources` / `baseline_types`) when
/// the network is unreachable on first run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub aql_key: String,
    pub label: String,
}

impl Choice {
    pub fn any(label: impl Into<String>) -> Self {
        Self {
            aql_key: String::new(),
            label: label.into(),
        }
    }

    pub fn new(aql_key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            aql_key: aql_key.into(),
            label: label.into(),
        }
    }

    pub fn is_any(&self) -> bool {
        self.aql_key.is_empty()
    }

    /// Hardcoded source list used when the server preset fetch hasn't
    /// populated yet (first run, offline, etc.). Order and keys mirror what
    /// the live server returned at the time of writing.
    pub fn baseline_sources() -> Vec<Choice> {
        let mut v = vec![Choice::any("Any source")];
        for (k, l) in [
            ("csdb", "CSDB"),
            ("hvsc", "HVSC"),
            ("c64com", "c64.com"),
            ("oneload", "OneLoad64"),
            ("gamebase", "Gamebase64"),
            ("c64orgintro", "c64.org Intros"),
            ("tapes", "c64Tapes.org"),
            ("seuck", "SEUCK"),
            ("mayhem", "Mayhem CRT"),
            ("pres", "Preservers"),
            ("utape", "Ultimate Tape Archive"),
            ("guybrush", "Guybrush"),
        ] {
            v.push(Choice::new(k, l));
        }
        v
    }

    pub fn baseline_types() -> Vec<Choice> {
        let mut v = vec![Choice::any("Any type")];
        for (k, l) in [
            ("demos", "Demos"),
            ("games", "Games"),
            ("intros", "Intros"),
            ("music", "Music"),
            ("graphics", "Graphics"),
            ("tools", "Tools"),
            ("mags", "Mags"),
            ("charts", "Charts"),
            ("bbs", "BBS"),
            ("c128", "C128"),
            ("easyflash", "EasyFlash"),
            ("misc", "Misc"),
            ("reu", "REU"),
        ] {
            v.push(Choice::new(k, l));
        }
        v
    }
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// One subcategory entry from `/search/aql/presets` — has a numeric `id`
/// matching the `category_id` field on entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subcategory {
    pub id: u16,
    pub aql_key: String,
    pub name: String,
}

/// All three preset lists from `/search/aql/presets`.
///
/// `sources` and `types` always include the leading "Any" choice so they're
/// drop-in for `pick_list` without further wrapping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Presets {
    pub sources: Vec<Choice>,
    pub types: Vec<Choice>,
    pub subcats: Vec<Subcategory>,
}

impl Presets {
    /// Hardcoded baseline used when the server fetch hasn't returned yet.
    pub fn baseline() -> Self {
        Self {
            sources: Choice::baseline_sources(),
            types: Choice::baseline_types(),
            subcats: Vec::new(),
        }
    }
}

/// One row from `/search/categories` — id ↔ source ↔ display label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryInfo {
    pub id: u16,
    /// AQL key (e.g. "demos", "c64comdemos").
    pub name: String,
    /// Human-readable label (e.g. "c64.com demos").
    pub description: String,
    /// Top-level grouping (e.g. "Demos", "Games").
    pub grouping_name: String,
    /// Repository / source key (e.g. "csdb", "c64com").
    pub source_type: String,
}

/// Live category-id → label map. Falls back to the hardcoded
/// [`category_label`] for ids the server didn't return (or when the registry
/// hasn't been fetched yet).
#[derive(Debug, Clone, Default)]
pub struct CategoryRegistry {
    map: std::collections::HashMap<u16, CategoryInfo>,
}

impl CategoryRegistry {
    pub fn new(entries: Vec<CategoryInfo>) -> Self {
        Self {
            map: entries.into_iter().map(|c| (c.id, c)).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Human-readable label for a category id. Tries the live registry
    /// first; falls back to the hardcoded table; finally returns the
    /// numeric id as a string so the UI never shows a blank cell.
    pub fn label(&self, id: u16) -> String {
        if let Some(info) = self.map.get(&id) {
            // Server returns lowercase descriptions like "c64.com demos" —
            // capitalise the first letter for display consistency.
            return capitalize_first(&info.description);
        }
        let fallback = category_label(id);
        if fallback.is_empty() {
            format!("category {}", id)
        } else {
            fallback.to_string()
        }
    }

    /// Source type (e.g. "csdb", "hvsc") for a category id, when known.
    pub fn source_type(&self, id: u16) -> Option<&str> {
        self.map.get(&id).map(|c| c.source_type.as_str())
    }

    pub fn entries(&self) -> impl Iterator<Item = &CategoryInfo> {
        self.map.values()
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingFilter {
    Any,
    AtLeast(u8),
    Exactly10,
}

impl RatingFilter {
    pub const ALL: [RatingFilter; 5] = [
        RatingFilter::Any,
        RatingFilter::AtLeast(7),
        RatingFilter::AtLeast(8),
        RatingFilter::AtLeast(9),
        RatingFilter::Exactly10,
    ];

    fn aql_fragment(self) -> Option<String> {
        match self {
            RatingFilter::Any => None,
            RatingFilter::AtLeast(n) => Some(format!("rating:>={}", n)),
            RatingFilter::Exactly10 => Some("rating:10".to_string()),
        }
    }
}

impl std::fmt::Display for RatingFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RatingFilter::Any => f.write_str("Any rating"),
            RatingFilter::AtLeast(n) => write!(f, "{}+ stars", n),
            RatingFilter::Exactly10 => f.write_str("10 only"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyFilter {
    Any,
    Day,
    Week,
    Month,
}

impl RecencyFilter {
    pub const ALL: [RecencyFilter; 4] = [
        RecencyFilter::Any,
        RecencyFilter::Day,
        RecencyFilter::Week,
        RecencyFilter::Month,
    ];

    fn aql_fragment(self) -> Option<&'static str> {
        // The server's recency filter is the `latest:` qualifier with a
        // server-defined window key. Singular for "1" (`1week`, `1month`),
        // plural for higher counts. Confirmed against `/search/aql/presets`
        // → group `latest`. A bare `1month` (without the qualifier) returns
        // HTTP 463, so the `latest:` prefix is mandatory.
        match self {
            RecencyFilter::Any => None,
            RecencyFilter::Day => Some("latest:1days"),
            RecencyFilter::Week => Some("latest:1week"),
            RecencyFilter::Month => Some("latest:1month"),
        }
    }
}

impl std::fmt::Display for RecencyFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RecencyFilter::Any => "Any time",
            RecencyFilter::Day => "Last day",
            RecencyFilter::Week => "Last week",
            RecencyFilter::Month => "Last month",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Newest entries first. The default — matches the empty-form
    /// fallback so "no filters at all" and "default sort" mean the same
    /// thing visually and on the wire.
    LatestFirst,
    RatingDesc,
    YearDesc,
}

impl SortOrder {
    pub const ALL: [SortOrder; 3] = [
        SortOrder::LatestFirst,
        SortOrder::RatingDesc,
        SortOrder::YearDesc,
    ];

    fn aql_fragment(self) -> Option<&'static str> {
        match self {
            SortOrder::LatestFirst => Some("sort:updated order:desc"),
            SortOrder::RatingDesc => Some("sort:rating order:desc"),
            SortOrder::YearDesc => Some("sort:year order:desc"),
        }
    }
}

impl std::fmt::Display for SortOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SortOrder::LatestFirst => "Latest first",
            SortOrder::RatingDesc => "Rating ↓",
            SortOrder::YearDesc => "Year ↓",
        };
        f.write_str(s)
    }
}

/// Form state composed by the search bar UI. `compose_aql` turns this into
/// the wire-format AQL string passed to `Assembly64Client::search`.
///
/// `type_filter` and `source_filter` are dynamic [`Choice`] values populated
/// from `/search/aql/presets` (or the hardcoded baseline at first run).
#[derive(Debug, Clone)]
pub struct SearchForm {
    pub free_text: String,
    pub type_filter: Choice,
    pub source_filter: Choice,
    pub rating_filter: RatingFilter,
    pub recency_filter: RecencyFilter,
    pub sort_order: SortOrder,
}

impl Default for SearchForm {
    fn default() -> Self {
        Self {
            free_text: String::new(),
            type_filter: Choice::any("Any type"),
            source_filter: Choice::any("Any source"),
            rating_filter: RatingFilter::default(),
            recency_filter: RecencyFilter::default(),
            sort_order: SortOrder::default(),
        }
    }
}

impl Default for RatingFilter {
    fn default() -> Self {
        RatingFilter::Any
    }
}
impl Default for RecencyFilter {
    fn default() -> Self {
        RecencyFilter::Any
    }
}
impl Default for SortOrder {
    fn default() -> Self {
        SortOrder::LatestFirst
    }
}

/// AQL the server falls back to when the form has no other constraints.
/// The API rejects an empty `query=` parameter (HTTP 463), and a single
/// bare repo keyword (e.g. `csdb`) on its own is also rejected — at least
/// one sort/filter expression is required, so we always end up with this
/// "show me the latest entries" baseline.
const DEFAULT_LATEST_AQL: &str = "sort:updated order:desc";

impl SearchForm {
    /// Compose this form into a single AQL query string.
    ///
    /// Free text containing `:` is treated as raw AQL and passed through
    /// verbatim; otherwise it's wrapped as `name:*…*` so a typed word
    /// becomes a substring search.
    ///
    /// If the form would produce no constraints at all, falls back to
    /// `sort:updated order:desc` (the API rejects empty queries).
    pub fn compose_aql(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        let trimmed = self.free_text.trim();
        if !trimmed.is_empty() {
            if trimmed.contains(':') {
                parts.push(trimmed.to_string());
            } else {
                // Multi-word terms must use `*` as the separator inside the
                // `name:*…*` wildcard — the server's AQL parser rejects a
                // literal space between two words (HTTP 463), even though
                // entries with spaces in their names are common ("Bubble
                // Bobble" etc.). Collapsing runs of whitespace to a single
                // `*` matches space, underscore, hyphen, or any other
                // separator the title might use.
                let glued = trimmed.split_whitespace().collect::<Vec<_>>().join("*");
                parts.push(format!("name:*{}*", glued));
            }
        }
        // The `aqlKey` from /search/aql/presets is the *value* half of a
        // `key:value` pair — sending the bare token returns HTTP 463.
        // Each preset group has a fixed AQL key prefix:
        //   repo:<name>      (sources)
        //   category:<name>  (types)
        //   subcat:<name>    (subcategories — not surfaced yet)
        if !self.type_filter.is_any() {
            parts.push(format!("category:{}", self.type_filter.aql_key));
        }
        if !self.source_filter.is_any() {
            parts.push(format!("repo:{}", self.source_filter.aql_key));
        }
        if let Some(r) = self.rating_filter.aql_fragment() {
            parts.push(r);
        }
        if let Some(r) = self.recency_filter.aql_fragment() {
            parts.push(r.to_string());
        }
        if let Some(s) = self.sort_order.aql_fragment() {
            parts.push(s.to_string());
        }

        if parts.is_empty() {
            DEFAULT_LATEST_AQL.to_string()
        } else {
            parts.join(" ")
        }
    }
}

// -----------------------------------------------------------------------------
// AQL URL encoder
// -----------------------------------------------------------------------------

/// Percent-encode AQL for the `?query=` parameter.
///
/// Ported from `../u64ctl/src/u64mui/assembly64.c::url_encode` (lines 74–96):
/// escape space, `"`, `#`, `&`, `+`, `%`, `<`, `=`, `>`, control bytes, and
/// any non-ASCII byte. **Leave `:` and `*` alone** so AQL stays readable on
/// the wire (e.g. `name:*elite*`). nginx rejects raw `>=` so encoding `<`,
/// `=`, `>` is required for `rating:>=7` filters.
pub fn encode_aql(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        let must_escape = matches!(
            b,
            b' ' | b'"' | b'#' | b'&' | b'+' | b'%' | b'<' | b'=' | b'>'
        ) || b < 0x20
            || b >= 0x7F;
        if must_escape {
            out.push(b'%');
            out.push(HEX[((b >> 4) & 0x0F) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        } else {
            out.push(b);
        }
    }
    // Safe: only ASCII / percent-escapes were pushed.
    unsafe { String::from_utf8_unchecked(out) }
}

// -----------------------------------------------------------------------------
// Wire format helpers
// -----------------------------------------------------------------------------

/// Raw search-result JSON shape. Some fields the server omits — keep
/// everything optional and we'll fill defaults at the boundary.
#[derive(Debug, Clone, Deserialize)]
struct WireEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    category: u16,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    handle: Option<String>,
    /// Year is sometimes 0 on the wire — treat as missing.
    #[serde(default)]
    year: Option<u16>,
    #[serde(default)]
    rating: Option<u8>,
    #[serde(default, rename = "siteRating")]
    site_rating: Option<f32>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    released: Option<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    place: Option<u16>,
    #[serde(default)]
    compo: Option<u16>,
    #[serde(default, rename = "siteCategory")]
    site_category: Option<u16>,
}

impl From<WireEntry> for AsmEntry {
    fn from(w: WireEntry) -> Self {
        AsmEntry {
            item_id: w.id,
            category_id: w.category,
            name: w.name,
            group: w.group.filter(|s| !s.is_empty()),
            handle: w.handle.filter(|s| !s.is_empty()),
            year: w.year.filter(|&y| y > 0),
            rating: w.rating,
            site_rating: w.site_rating.filter(|&r| r > 0.0),
            updated: w.updated.filter(|s| !s.is_empty()),
            released: w.released.filter(|s| !s.is_empty()),
            event: w.event.filter(|s| !s.is_empty()),
            place: w.place.filter(|&p| p > 0),
            // The server uses compo id 0 for "C64 DEMO" — a real value, not
            // a sentinel — so don't filter it out the way we do for `year`
            // or `place`. Caller decides whether to look it up.
            compo: w.compo,
            site_category: w.site_category.filter(|&c| c > 0),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireFilesEnvelope {
    #[serde(default, rename = "contentEntry")]
    content_entry: Vec<WireFile>,
}

#[derive(Debug, Deserialize)]
struct WireFile {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    size: u64,
}

impl From<WireFile> for AsmFile {
    fn from(w: WireFile) -> Self {
        AsmFile {
            file_id: w.id,
            path: w.path,
            size_bytes: w.size,
        }
    }
}

// -----------------------------------------------------------------------------
// Client
// -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct Assembly64Client {
    http: Client,
    base: String,
}

impl Assembly64Client {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            base: BASE_URL.to_string(),
        }
    }

    /// Build a default client tuned for the Assembly64 API. Reuses
    /// [`crate::net_utils::build_external_client`] so timeout/UA policy
    /// is consistent with other external API calls.
    pub fn with_defaults(user_agent: &str) -> Result<Self> {
        let http = crate::net_utils::build_external_client(user_agent, 30)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(Self::new(http))
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.http.get(url).header("client-id", CLIENT_ID)
    }

    /// Paged AQL search. `query` is the assembled AQL string; pass an empty
    /// string to get the latest entries.
    ///
    /// Cross-repo dedup runs on the returned page only — the same release
    /// frequently appears multiple times via different category bindings,
    /// and clicking a duplicate with a non-matching category returns
    /// HTTP 500 (see u64ctl `assembly64.c:389-414`).
    pub async fn search(
        &self,
        query: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<AsmEntry>, AssemblyError> {
        let url = format!(
            "{}/search/aql/{}/{}?query={}",
            self.base,
            offset,
            limit,
            encode_aql(query)
        );
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            // 463 = AQL syntax error — surface as a typed error so the
            // UI can show an inline hint instead of a generic failure.
            if status.as_u16() == 463 {
                return Err(AssemblyError::AqlSyntax);
            }
            return Err(AssemblyError::Http(status));
        }

        let body = resp.bytes().await?;
        if body.is_empty() {
            return Ok(Vec::new());
        }

        let raw: Vec<WireEntry> = serde_json::from_slice(&body)?;
        let mut out: Vec<AsmEntry> = raw.into_iter().map(AsmEntry::from).collect();
        dedup_by_name_group_year(&mut out);
        Ok(out)
    }

    /// List files in one entry. Returns the inner `contentEntry` array.
    pub async fn list_files(
        &self,
        item_id: &str,
        category_id: u16,
    ) -> Result<Vec<AsmFile>, AssemblyError> {
        let url = format!("{}/search/entries/{}/{}", self.base, item_id, category_id);
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let envelope: WireFilesEnvelope = resp.json().await?;
        Ok(envelope
            .content_entry
            .into_iter()
            .map(AsmFile::from)
            .collect())
    }

    /// Download a single file. Returns the raw bytes — the caller decides
    /// whether to write to disk, hand to the Ultimate64 directly, or pipe
    /// into the ZIP extractor.
    pub async fn download(
        &self,
        item_id: &str,
        category_id: u16,
        file_id: u64,
    ) -> Result<Vec<u8>, AssemblyError> {
        let url = format!(
            "{}/search/bin/{}/{}/{}",
            self.base, item_id, category_id, file_id
        );
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// Fetch dropdown presets from `/search/aql/presets`.
    ///
    /// The response groups options by `type` ("repo" / "category" / "subcat");
    /// we flatten that into the [`Presets`] shape with leading "Any" rows
    /// already inserted so the UI can use the result directly.
    pub async fn presets(&self) -> Result<Presets, AssemblyError> {
        let url = format!("{}/search/aql/presets", self.base);
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let groups: Vec<WirePresetGroup> = resp.json().await?;

        let mut sources = vec![Choice::any("Any source")];
        let mut types = vec![Choice::any("Any type")];
        let mut subcats: Vec<Subcategory> = Vec::new();

        for group in groups {
            match group.r#type.as_str() {
                "repo" => {
                    for v in group.values {
                        sources.push(Choice::new(v.aql_key, v.name));
                    }
                }
                "category" => {
                    for v in group.values {
                        types.push(Choice::new(v.aql_key, v.name));
                    }
                }
                "subcat" => {
                    for v in group.values {
                        if let Some(id) = v.id {
                            subcats.push(Subcategory {
                                id,
                                aql_key: v.aql_key,
                                name: v.name,
                            });
                        }
                    }
                }
                _ => {} // Future preset groups: ignore quietly.
            }
        }

        Ok(Presets {
            sources,
            types,
            subcats,
        })
    }

    /// Fetch the full category table from `/search/categories`.
    ///
    /// Returns one [`CategoryInfo`] per known category id, with the source
    /// repo (`type` field on the wire) and human label preserved for
    /// runtime lookup. Cache and reuse — the table rarely changes.
    pub async fn category_registry(&self) -> Result<CategoryRegistry, AssemblyError> {
        let url = format!("{}/search/categories", self.base);
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let body = resp.text().await?;

        // Documented shape: array of `{id, name, description, groupingName, type}`.
        if let Ok(arr) = serde_json::from_str::<Vec<WireCategoryRow>>(&body) {
            let entries: Vec<CategoryInfo> = arr
                .into_iter()
                .map(|c| CategoryInfo {
                    id: c.id,
                    name: c.name,
                    description: c.description,
                    grouping_name: c.grouping_name,
                    source_type: c.r#type,
                })
                .collect();
            return Ok(CategoryRegistry::new(entries));
        }

        // Fallback for older deployments that returned a flat `{id: name}` map.
        if let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, String>>(&body) {
            let entries: Vec<CategoryInfo> = map
                .into_iter()
                .filter_map(|(k, v)| {
                    k.parse::<u16>().ok().map(|id| CategoryInfo {
                        id,
                        name: v.clone(),
                        description: v,
                        grouping_name: String::new(),
                        source_type: String::new(),
                    })
                })
                .collect();
            return Ok(CategoryRegistry::new(entries));
        }

        Err(AssemblyError::Other(
            "unrecognised /search/categories response shape".into(),
        ))
    }

    /// Fetch full metadata for one entry — same shape as a search hit but
    /// guaranteed to be complete. Used by the detail view when the source
    /// stub (e.g. a favorite re-opened across sessions) lacks fields like
    /// `handle`, `event`, or `site_rating`.
    pub async fn metadata(
        &self,
        item_id: &str,
        category_id: u16,
    ) -> Result<AsmEntry, AssemblyError> {
        let url = format!("{}/search/meta/{}/{}", self.base, item_id, category_id);
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let wire: WireEntry = resp.json().await?;
        Ok(AsmEntry::from(wire))
    }

    /// Fetch the compo-type id → label table from `/search/compotypes`.
    /// Stable enough to cache for the lifetime of the app.
    pub async fn compo_types(&self) -> Result<Vec<CompoType>, AssemblyError> {
        let url = format!("{}/search/compotypes", self.base);
        let resp = self.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(AssemblyError::Http(status));
        }
        let arr: Vec<WireCompoType> = resp.json().await?;
        Ok(arr
            .into_iter()
            .map(|c| CompoType {
                id: c.id,
                name: c.name,
            })
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct WirePresetGroup {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    values: Vec<WirePresetValue>,
}

#[derive(Debug, Deserialize)]
struct WirePresetValue {
    #[serde(default, rename = "aqlKey")]
    aql_key: String,
    #[serde(default)]
    name: String,
    /// Only populated for `subcat` rows.
    #[serde(default)]
    id: Option<u16>,
}

/// One compo-type row from `/search/compotypes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompoType {
    pub id: u16,
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct WireCompoType {
    #[serde(default)]
    id: u16,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct WireCategoryRow {
    #[serde(default)]
    id: u16,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "groupingName")]
    grouping_name: String,
    #[serde(default)]
    r#type: String,
}

// -----------------------------------------------------------------------------
// Dedup + size formatting
// -----------------------------------------------------------------------------

/// Drop subsequent entries that share `(name, group, year)` with an earlier
/// entry on the same page. This matches u64ctl's behavior — same release
/// from different repos collapses to a single row, and the **first** one
/// wins so the surfaced category_id corresponds to a working download URL.
fn dedup_by_name_group_year(entries: &mut Vec<AsmEntry>) {
    let mut seen: HashSet<(String, String, u16)> = HashSet::new();
    entries.retain(|e| {
        let key = (
            e.name.clone(),
            e.group.clone().unwrap_or_default(),
            e.year.unwrap_or(0),
        );
        seen.insert(key)
    });
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} B", bytes)
    }
}

/// Render a 0..=10 rating as a 10-glyph star bar. `None` → ten dim glyphs.
pub fn rating_stars(rating: Option<u8>) -> String {
    let r = rating.unwrap_or(0).min(10) as usize;
    let mut s = String::with_capacity(10 * 3);
    for i in 0..10 {
        s.push_str(if i < r { "★" } else { "☆" });
    }
    s
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_aql_preserves_aql_punctuation() {
        // `:` and `*` must NOT be escaped — they're core AQL syntax.
        assert_eq!(encode_aql("name:*elite*"), "name:*elite*");
    }

    #[test]
    fn encode_aql_escapes_space_and_comparison() {
        // `>=` must be escaped or nginx returns 400.
        assert_eq!(
            encode_aql("rating:>=7 sort:rating"),
            "rating:%3E%3D7%20sort:rating"
        );
    }

    #[test]
    fn encode_aql_escapes_special_chars() {
        assert_eq!(encode_aql("a&b#c+d%e\""), "a%26b%23c%2Bd%25e%22");
    }

    #[test]
    fn encode_aql_escapes_non_ascii() {
        // Non-ASCII bytes (e.g. UTF-8 bytes of "é") get percent-encoded.
        let encoded = encode_aql("café");
        // "café" = 63 61 66 c3 a9 → "caf%C3%A9"
        assert_eq!(encoded, "caf%C3%A9");
    }

    #[test]
    fn encode_aql_passes_alphanumerics_through() {
        assert_eq!(encode_aql("Hello123"), "Hello123");
    }

    #[test]
    fn compose_aql_wraps_plain_text() {
        // Default sort (LatestFirst) is always appended, so callers see
        // newest results unless they pick a different sort explicitly.
        let form = SearchForm {
            free_text: "elite".to_string(),
            ..Default::default()
        };
        assert_eq!(form.compose_aql(), "name:*elite* sort:updated order:desc");
    }

    #[test]
    fn compose_aql_glues_multi_word_with_wildcard() {
        // The server returns HTTP 463 for a literal space inside the name
        // wildcard (`name:*bubble bobble*`), so multi-word input must be
        // glued with `*` to match the title regardless of separator.
        let form = SearchForm {
            free_text: "bubble bobble".to_string(),
            ..Default::default()
        };
        assert_eq!(
            form.compose_aql(),
            "name:*bubble*bobble* sort:updated order:desc"
        );

        // Whitespace runs collapse cleanly.
        let form = SearchForm {
            free_text: "  space   invaders  64 ".to_string(),
            ..Default::default()
        };
        assert_eq!(
            form.compose_aql(),
            "name:*space*invaders*64* sort:updated order:desc"
        );
    }

    #[test]
    fn compose_aql_passes_through_explicit_aql() {
        // Power users typing `:` should bypass the substring wrap.
        let form = SearchForm {
            free_text: "group:fairlight".to_string(),
            ..Default::default()
        };
        assert_eq!(
            form.compose_aql(),
            "group:fairlight sort:updated order:desc"
        );
    }

    #[test]
    fn compose_aql_combines_filters() {
        let form = SearchForm {
            free_text: "elite".to_string(),
            type_filter: Choice::new("demos", "Demos"),
            source_filter: Choice::new("csdb", "CSDB"),
            rating_filter: RatingFilter::AtLeast(8),
            recency_filter: RecencyFilter::Month,
            sort_order: SortOrder::RatingDesc,
        };
        assert_eq!(
            form.compose_aql(),
            "name:*elite* category:demos repo:csdb rating:>=8 latest:1month sort:rating order:desc"
        );
    }

    #[test]
    fn baseline_choices_lead_with_any() {
        let sources = Choice::baseline_sources();
        assert!(sources.first().map(|c| c.is_any()).unwrap_or(false));
        let types = Choice::baseline_types();
        assert!(types.first().map(|c| c.is_any()).unwrap_or(false));
    }

    #[test]
    fn category_registry_falls_back_to_hardcoded_table() {
        let reg = CategoryRegistry::default();
        // Empty registry — hardcoded fallback for known id.
        assert_eq!(reg.label(1), "CSDB Demos");
        // Unknown id — last-resort numeric label.
        assert_eq!(reg.label(9999), "category 9999");
    }

    /// Parse the live `/search/aql/presets` body (captured 2026-04-25) the
    /// same way the client would. Guards against silent wire-format drift
    /// — if the server changes shape, this test breaks loudly.
    #[test]
    fn parse_live_presets_fixture() {
        let body = include_str!("../tests/fixtures/presets.json");
        let groups: Vec<WirePresetGroup> = serde_json::from_str(body).unwrap();
        // Server currently returns 9 groups; we only consume 3 of them
        // (repo, category, subcat), but parsing must succeed for all.
        assert!(
            groups.len() >= 3,
            "expected ≥3 preset groups, got {}",
            groups.len()
        );

        // Reconstruct what `Assembly64Client::presets` would assemble.
        let mut sources = vec![Choice::any("Any source")];
        let mut types = vec![Choice::any("Any type")];
        let mut subcats: Vec<Subcategory> = Vec::new();
        for g in groups {
            match g.r#type.as_str() {
                "repo" => {
                    sources.extend(g.values.into_iter().map(|v| Choice::new(v.aql_key, v.name)))
                }
                "category" => {
                    types.extend(g.values.into_iter().map(|v| Choice::new(v.aql_key, v.name)))
                }
                "subcat" => {
                    for v in g.values {
                        if let Some(id) = v.id {
                            subcats.push(Subcategory {
                                id,
                                aql_key: v.aql_key,
                                name: v.name,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        // Spot-check a known-stable entry.
        assert!(sources
            .iter()
            .any(|c| c.aql_key == "csdb" && c.label == "CSDB"));
        assert!(types
            .iter()
            .any(|c| c.aql_key == "demos" && c.label == "Demos"));
        // Subcategories carry numeric ids that match `category_id` on entries.
        assert!(subcats.iter().any(|s| s.id == 1));
    }

    #[test]
    fn parse_live_categories_fixture() {
        let body = include_str!("../tests/fixtures/categories.json");
        let arr: Vec<WireCategoryRow> = serde_json::from_str(body).unwrap();
        let registry = CategoryRegistry::new(
            arr.into_iter()
                .map(|c| CategoryInfo {
                    id: c.id,
                    name: c.name,
                    description: c.description,
                    grouping_name: c.grouping_name,
                    source_type: c.r#type,
                })
                .collect(),
        );
        // Live label for CSDB demos beats the hardcoded "CSDB Demos".
        let label = registry.label(1);
        assert!(!label.is_empty(), "label(1) returned empty string");
        assert_eq!(registry.source_type(1), Some("csdb"));
        assert_eq!(registry.source_type(33), Some("oneload"));
    }

    #[test]
    fn category_registry_prefers_live_data() {
        let reg = CategoryRegistry::new(vec![CategoryInfo {
            id: 1,
            name: "demos".into(),
            description: "csdb demos".into(),
            grouping_name: "Demos".into(),
            source_type: "csdb".into(),
        }]);
        // Live entry overrides the hardcoded "CSDB Demos" label, with a
        // capitalised first letter for display.
        assert_eq!(reg.label(1), "Csdb demos");
        assert_eq!(reg.source_type(1), Some("csdb"));
        // Unknown ids still fall through to hardcoded.
        assert_eq!(reg.label(33), "OneLoad64 Games");
    }

    #[test]
    fn compose_aql_default_form_is_latest_first() {
        // Empty `query=` triggers HTTP 463 server-side; the form's default
        // sort (LatestFirst) is what guarantees we always emit something
        // non-empty AND meaningful (newest entries first).
        let form = SearchForm::default();
        assert_eq!(form.compose_aql(), DEFAULT_LATEST_AQL);
    }

    fn make_entry(id: &str, category_id: u16, name: &str, year: Option<u16>) -> AsmEntry {
        AsmEntry {
            item_id: id.into(),
            category_id,
            name: name.into(),
            group: Some("Group".into()),
            handle: None,
            year,
            rating: Some(8),
            site_rating: None,
            updated: None,
            released: None,
            event: None,
            place: None,
            compo: None,
            site_category: None,
        }
    }

    #[test]
    fn dedup_keeps_first_occurrence() {
        let mut entries = vec![
            make_entry("1", 1, "Demo", Some(2024)),
            // Duplicate from a different repo — must be dropped.
            make_entry("2", 33, "Demo", Some(2024)),
            // Different year — kept.
            make_entry("3", 1, "Demo", Some(2023)),
        ];
        dedup_by_name_group_year(&mut entries);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].item_id, "1");
        assert_eq!(entries[1].item_id, "3");
    }

    #[test]
    fn csdb_categories_match_reference() {
        // From u64ctl assembly64.c:587-596.
        for id in 0..=10u16 {
            assert!(is_csdb_category(id), "id {} must be CSDB", id);
        }
        assert!(is_csdb_category(25));
        assert!(!is_csdb_category(11));
        assert!(!is_csdb_category(33));
    }

    #[test]
    fn rating_stars_rendering() {
        let s = rating_stars(Some(7));
        // 7 filled + 3 empty = 10 chars (each glyph is 3 UTF-8 bytes).
        assert_eq!(s.chars().count(), 10);
        assert_eq!(s.chars().filter(|&c| c == '★').count(), 7);
        assert_eq!(s.chars().filter(|&c| c == '☆').count(), 3);
    }

    #[test]
    fn rating_stars_handles_none_and_overflow() {
        assert_eq!(rating_stars(None).chars().count(), 10);
        // Out-of-range clamps to 10.
        let s = rating_stars(Some(99));
        assert_eq!(s.chars().filter(|&c| c == '★').count(), 10);
    }

    #[test]
    fn format_size_thresholds() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn entry_csdb_url_only_for_csdb_sources() {
        let csdb = make_entry("12345", 1, "x", None);
        assert_eq!(
            csdb.csdb_release_url().as_deref(),
            Some("https://csdb.dk/release/?id=12345")
        );
        let oneload = AsmEntry {
            category_id: 33,
            ..csdb.clone()
        };
        assert!(oneload.csdb_release_url().is_none());
    }
}
