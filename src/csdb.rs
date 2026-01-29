//! CSDb API module for fetching releases, searching, and downloading files.
//!
//! This module provides functionality to interact with CSDb (https://csdb.dk):
//! - Search for releases
//! - Get latest releases
//! - List downloadable files for a release
//! - Download files
//! - Extract ZIP archives

use anyhow::{Context, Result};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use url::Url;
use zip::ZipArchive;

// -----------------------------------------------------------------------------
// Data structures
// -----------------------------------------------------------------------------

/// Default delay between requests in milliseconds (1.5 seconds)
const REQUEST_DELAY_MS: u64 = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCategory {
    #[default]
    All,
    Releases,
    Groups,
    Sids,
}

impl SearchCategory {
    pub fn as_param(&self) -> &'static str {
        match self {
            SearchCategory::All => "all",
            SearchCategory::Releases => "releases",
            SearchCategory::Groups => "groups",
            SearchCategory::Sids => "sids",
        }
    }

    pub fn all_categories() -> Vec<SearchCategory> {
        vec![
            SearchCategory::All,
            SearchCategory::Releases,
            SearchCategory::Groups,
            SearchCategory::Sids,
        ]
    }
}

impl std::fmt::Display for SearchCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchCategory::All => write!(f, "All"),
            SearchCategory::Releases => write!(f, "Releases"),
            SearchCategory::Groups => write!(f, "Groups"),
            SearchCategory::Sids => write!(f, "SIDs"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TopListCategory {
    All,
    BbsGraphics,
    C128Release,
    C64_1kIntro,
    C64_256bIntro,
    C64_4kGame,
    C64_4kIntro,
    C64BasicDemo,
    C64Crack,
    C64CrackIntro,
    #[default]
    C64Demo,
    C64DiskCover,
    C64Diskmag,
    C64Dtv,
    C64FakeDemo,
    C64Game,
    C64GamePreview,
    C64Graphics,
    C64GraphicsCollection,
    C64Intro,
    C64IntroCollection,
    C64Invitation,
    C64Misc,
    C64Music,
    C64MusicCollection,
    C64OneFileDemo,
    C64Papermag,
    C64Tool,
    EasyFlashRelease,
    OtherPlatformC64Tool,
    ReuRelease,
}

impl TopListCategory {
    pub fn as_param(&self) -> &'static str {
        match self {
            TopListCategory::All => "",
            TopListCategory::BbsGraphics => "(43)",
            TopListCategory::C128Release => "(27)",
            TopListCategory::C64_1kIntro => "(18)",
            TopListCategory::C64_256bIntro => "(36)",
            TopListCategory::C64_4kGame => "(35)",
            TopListCategory::C64_4kIntro => "(4)",
            TopListCategory::C64BasicDemo => "(22)",
            TopListCategory::C64Crack => "(20)",
            TopListCategory::C64CrackIntro => "(5)",
            TopListCategory::C64Demo => "(1)",
            TopListCategory::C64DiskCover => "(33)",
            TopListCategory::C64Diskmag => "(13)",
            TopListCategory::C64Dtv => "(40)",
            TopListCategory::C64FakeDemo => "(24)",
            TopListCategory::C64Game => "(11)",
            TopListCategory::C64GamePreview => "(19)",
            TopListCategory::C64Graphics => "(9)",
            TopListCategory::C64GraphicsCollection => "(10)",
            TopListCategory::C64Intro => "(3)",
            TopListCategory::C64IntroCollection => "(44)",
            TopListCategory::C64Invitation => "(16)",
            TopListCategory::C64Misc => "(17)",
            TopListCategory::C64Music => "(7)",
            TopListCategory::C64MusicCollection => "(8)",
            TopListCategory::C64OneFileDemo => "(2)",
            TopListCategory::C64Papermag => "(26)",
            TopListCategory::C64Tool => "(15)",
            TopListCategory::EasyFlashRelease => "(46)",
            TopListCategory::OtherPlatformC64Tool => "(21)",
            TopListCategory::ReuRelease => "(6)",
        }
    }

    pub fn all_categories() -> Vec<TopListCategory> {
        vec![
            TopListCategory::C64Demo,
            TopListCategory::C64OneFileDemo,
            TopListCategory::C64Intro,
            TopListCategory::C64_4kIntro,
            TopListCategory::C64_1kIntro,
            TopListCategory::C64_256bIntro,
            TopListCategory::C64Game,
            TopListCategory::C64_4kGame,
            TopListCategory::C64Music,
            TopListCategory::C64MusicCollection,
            TopListCategory::C64Graphics,
            TopListCategory::C64GraphicsCollection,
            TopListCategory::C64CrackIntro,
            TopListCategory::C64Crack,
            TopListCategory::C64Diskmag,
            TopListCategory::C64Tool,
            TopListCategory::C64Invitation,
            TopListCategory::C64BasicDemo,
            TopListCategory::C64Misc,
            TopListCategory::C64DiskCover,
            TopListCategory::C64IntroCollection,
            TopListCategory::C64GamePreview,
            TopListCategory::C64FakeDemo,
            TopListCategory::C64Papermag,
            TopListCategory::C128Release,
            TopListCategory::ReuRelease,
            TopListCategory::EasyFlashRelease,
            TopListCategory::C64Dtv,
            TopListCategory::BbsGraphics,
            TopListCategory::OtherPlatformC64Tool,
            TopListCategory::All,
        ]
    }
}

impl std::fmt::Display for TopListCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopListCategory::All => write!(f, "All"),
            TopListCategory::BbsGraphics => write!(f, "BBS Graphics"),
            TopListCategory::C128Release => write!(f, "C128 Release"),
            TopListCategory::C64_1kIntro => write!(f, "C64 1K Intro"),
            TopListCategory::C64_256bIntro => write!(f, "C64 256b Intro"),
            TopListCategory::C64_4kGame => write!(f, "C64 4K Game"),
            TopListCategory::C64_4kIntro => write!(f, "C64 4K Intro"),
            TopListCategory::C64BasicDemo => write!(f, "C64 Basic Demo"),
            TopListCategory::C64Crack => write!(f, "C64 Crack"),
            TopListCategory::C64CrackIntro => write!(f, "C64 Crack Intro"),
            TopListCategory::C64Demo => write!(f, "C64 Demo"),
            TopListCategory::C64DiskCover => write!(f, "C64 Disk Cover"),
            TopListCategory::C64Diskmag => write!(f, "C64 Diskmag"),
            TopListCategory::C64Dtv => write!(f, "C64 DTV"),
            TopListCategory::C64FakeDemo => write!(f, "C64 Fake Demo"),
            TopListCategory::C64Game => write!(f, "C64 Game"),
            TopListCategory::C64GamePreview => write!(f, "C64 Game Preview"),
            TopListCategory::C64Graphics => write!(f, "C64 Graphics"),
            TopListCategory::C64GraphicsCollection => write!(f, "C64 Graphics Collection"),
            TopListCategory::C64Intro => write!(f, "C64 Intro"),
            TopListCategory::C64IntroCollection => write!(f, "C64 Intro Collection"),
            TopListCategory::C64Invitation => write!(f, "C64 Invitation"),
            TopListCategory::C64Misc => write!(f, "C64 Misc."),
            TopListCategory::C64Music => write!(f, "C64 Music"),
            TopListCategory::C64MusicCollection => write!(f, "C64 Music Collection"),
            TopListCategory::C64OneFileDemo => write!(f, "C64 One-File Demo"),
            TopListCategory::C64Papermag => write!(f, "C64 Papermag"),
            TopListCategory::C64Tool => write!(f, "C64 Tool"),
            TopListCategory::EasyFlashRelease => write!(f, "EasyFlash Release"),
            TopListCategory::OtherPlatformC64Tool => write!(f, "Other Platform C64 Tool"),
            TopListCategory::ReuRelease => write!(f, "REU Release"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopListEntry {
    pub rank: usize,
    pub release_id: Option<String>,
    pub title: String,
    pub release_url: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub release_id: Option<String>,
    pub title: String,
    pub release_url: String,
    pub group: Option<String>,
    pub release_type: Option<String>,
    pub year: Option<String>,
    pub exact_match: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestRelease {
    pub release_id: String,
    pub title: String,
    pub release_url: String,
    pub group: Option<String>,
    pub release_type: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseFile {
    pub index: usize,
    pub kind: String,       // "download" or "internal"
    pub id: Option<String>, // id from download.php (if present)
    pub url: String,        // the URL you can GET
    pub final_url: String,  // resolved final URL after redirects
    pub filename: String,   // from final_url path
    pub ext: String,        // lowercase extension without dot
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseDetails {
    pub release_id: String,
    pub title: String,
    pub group: Option<String>,
    pub release_type: Option<String>,
    pub release_date: Option<String>,
    pub platform: Option<String>,
    pub files: Vec<ReleaseFile>,
}

// -----------------------------------------------------------------------------
// ZIP extraction structures
// -----------------------------------------------------------------------------

/// Represents an extracted ZIP archive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedZip {
    pub source_filename: String,
    pub extract_dir: PathBuf,
    pub files: Vec<ExtractedFile>,
}

/// Represents a file extracted from a ZIP archive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFile {
    pub index: usize,
    pub filename: String,
    pub path: PathBuf,
    pub ext: String,
    pub size: u64,
}

// -----------------------------------------------------------------------------
// CSDb Client
// -----------------------------------------------------------------------------

pub struct CsdbClient {
    client: Client,
    last_request: std::sync::Mutex<Option<std::time::Instant>>,
}

impl CsdbClient {
    pub fn new() -> Result<Self> {
        use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONNECTION, HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"));
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.5"));
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));

        let client = Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .default_headers(headers)
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            last_request: std::sync::Mutex::new(None),
        })
    }

    /// Enforce rate limiting by waiting if necessary before making a request
    async fn rate_limit(&self) {
        let delay_needed = {
            let mut last = self.last_request.lock().unwrap();
            let now = std::time::Instant::now();
            let delay = if let Some(last_time) = *last {
                let elapsed = now.duration_since(last_time);
                let min_delay = std::time::Duration::from_millis(REQUEST_DELAY_MS);
                if elapsed < min_delay {
                    Some(min_delay - elapsed)
                } else {
                    None
                }
            } else {
                None
            };
            *last = Some(now);
            delay
        };

        if let Some(delay) = delay_needed {
            tokio::time::sleep(delay).await;
            // Update timestamp after sleeping
            let mut last = self.last_request.lock().unwrap();
            *last = Some(std::time::Instant::now());
        }
    }

    /// Search for releases on CSDb
    pub async fn search(
        &self,
        term: &str,
        category: SearchCategory,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.rate_limit().await;

        let encoded_term = urlencoding::encode(term);
        let url = format!(
            "https://csdb.dk/search/?seinsel={}&search={}&all=1",
            category.as_param(),
            encoded_term
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to perform search")?;

        response
            .error_for_status_ref()
            .context("Search HTTP error")?;

        let final_url = response.url().to_string();
        let html = response
            .text()
            .await
            .context("Failed to read search response")?;

        // Case 1: exact match -> redirected to a release page
        if final_url.contains("/release/?id=") {
            let id_re = Regex::new(r"id=(\d+)").unwrap();
            let rid = id_re.captures(&final_url).map(|c| c[1].to_string());

            // Try to extract title from the page
            let title = self
                .extract_title_from_html(&html)
                .unwrap_or_else(|| term.to_string());

            return Ok(vec![SearchResult {
                release_id: rid,
                title,
                release_url: final_url,
                group: None,
                release_type: None,
                year: None,
                exact_match: true,
            }]);
        }

        // Case 2: normal search page with list of results
        let mut results = self.parse_search_results(&html);

        if limit > 0 && results.len() > limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    /// Get latest releases from CSDb homepage
    pub async fn get_latest_releases(&self, limit: usize) -> Result<Vec<LatestRelease>> {
        self.rate_limit().await;

        let url = "https://csdb.dk/";
        let html = self.http_get(url).await?;

        let mut releases = self.parse_latest_releases(&html);

        if limit > 0 && releases.len() > limit {
            releases.truncate(limit);
        }

        Ok(releases)
    }

    /// Get top list from CSDb
    pub async fn get_top_list(
        &self,
        category: TopListCategory,
        limit: usize,
    ) -> Result<Vec<TopListEntry>> {
        self.rate_limit().await;

        let encoded_subtype = urlencoding::encode(category.as_param());
        let url = format!(
            "https://csdb.dk/toplist.php?type=release&subtype={}",
            encoded_subtype
        );

        let html = self.http_get(&url).await?;
        let mut entries = self.parse_top_list(&html);

        if limit > 0 && entries.len() > limit {
            entries.truncate(limit);
        }

        Ok(entries)
    }

    /// Get details and files for a specific release
    pub async fn get_release_details(&self, release_url: &str) -> Result<ReleaseDetails> {
        self.rate_limit().await;

        let html = self.http_get(release_url).await?;

        // Extract release ID from URL
        let id_re = Regex::new(r"id=(\d+)").unwrap();
        let release_id = id_re
            .captures(release_url)
            .map(|c| c[1].to_string())
            .unwrap_or_default();

        // Extract title
        let title = self
            .extract_title_from_html(&html)
            .unwrap_or_else(|| "Unknown".to_string());

        // Extract group
        let group = self.extract_group_from_html(&html);

        // Extract release type
        let release_type = self.extract_release_type_from_html(&html);

        // Extract release date
        let release_date = self.extract_date_from_html(&html);

        // Extract platform
        let platform = self.extract_platform_from_html(&html);

        // Get downloadable files
        let files = self.build_file_list(&html, release_url).await?;

        Ok(ReleaseDetails {
            release_id,
            title,
            group,
            release_type,
            release_date,
            platform,
            files,
        })
    }

    /// Download a file to the specified directory
    pub async fn download_file(&self, file: &ReleaseFile, out_dir: &PathBuf) -> Result<PathBuf> {
        self.rate_limit().await;

        fs::create_dir_all(out_dir)
            .await
            .with_context(|| format!("Failed to create directory {:?}", out_dir))?;

        // Use final_url which is already resolved
        let download_url = if file.final_url.starts_with("http") {
            file.final_url.clone()
        } else {
            self.resolve_final_url(&file.url).await?
        };

        let out_path = out_dir.join(&file.filename);

        let response = self
            .client
            .get(&download_url)
            .send()
            .await
            .with_context(|| format!("Failed to download {}", download_url))?;

        response
            .error_for_status_ref()
            .with_context(|| format!("HTTP error downloading {}", download_url))?;

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read bytes from {}", download_url))?;

        let mut out_file = File::create(&out_path)
            .await
            .with_context(|| format!("Failed to create file {:?}", out_path))?;

        out_file
            .write_all(&bytes)
            .await
            .with_context(|| format!("Failed to write to {:?}", out_path))?;

        Ok(out_path)
    }

    /// Download file and return bytes directly (for running without saving)
    pub async fn download_file_bytes(&self, file: &ReleaseFile) -> Result<(String, Vec<u8>)> {
        self.rate_limit().await;

        // Use final_url which is already resolved (avoids double resolution and handles pre-extracted URLs)
        let download_url = if file.final_url.starts_with("http") {
            file.final_url.clone()
        } else {
            // Fallback: resolve if final_url is not set properly
            self.resolve_final_url(&file.url).await?
        };

        let response = self
            .client
            .get(&download_url)
            .send()
            .await
            .with_context(|| format!("Failed to download {}", download_url))?;

        response
            .error_for_status_ref()
            .with_context(|| format!("HTTP error downloading {}", download_url))?;

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read bytes from {}", download_url))?;

        Ok((file.filename.clone(), bytes.to_vec()))
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    async fn http_get(&self, url: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch {}", url))?;

        response
            .error_for_status_ref()
            .with_context(|| format!("HTTP error for {}", url))?;

        response
            .text()
            .await
            .with_context(|| format!("Failed to read response from {}", url))
    }

    async fn resolve_final_url(&self, url: &str) -> Result<String> {
        self.rate_limit().await;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to resolve {}", url))?;

        response
            .error_for_status_ref()
            .with_context(|| format!("HTTP error resolving {}", url))?;

        Ok(response.url().to_string())
    }

    fn parse_search_results(&self, html: &str) -> Vec<SearchResult> {
        let mut results = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // First pass: find all release links - be flexible with attributes
        let release_re =
            Regex::new(r#"<a\s[^>]*href="/release/\?id=(\d+)"[^>]*>([^<]+)</a>"#).unwrap();

        // Regex for type: (C64 Something) or (C128 Something) etc.
        let type_re =
            Regex::new(r#"\((C64[^)]+|C128[^)]+|REU[^)]+|BBS[^)]+|Easy[^)]+|Other[^)]+)\)"#)
                .unwrap();

        // Regex for group/scener name inside font tag
        let group_re =
            Regex::new(r#"href="/(?:group|scener)/\?id=\d+"[^>]*><font[^>]*>([^<]+)</font>"#)
                .unwrap();

        // Alternative: group/scener without font tag
        let group_alt_re =
            Regex::new(r#"href="/(?:group|scener)/\?id=\d+"[^>]*>([^<]+)</a>"#).unwrap();

        let matches: Vec<_> = release_re.captures_iter(html).collect();

        for (i, cap) in matches.iter().enumerate() {
            let rid = cap[1].to_string();

            if seen.contains(&rid) {
                continue;
            }
            seen.insert(rid.clone());

            let title = strip_tags(&cap[2]);

            // Get context: from end of this match to start of next match (or +500 chars)
            let match_end = cap.get(0).unwrap().end();
            let next_match_start = matches
                .get(i + 1)
                .map(|m| m.get(0).unwrap().start())
                .unwrap_or(html.len());
            let context_end = match_end + (500.min(next_match_start.saturating_sub(match_end)));
            let context = &html[match_end..context_end.min(html.len())];

            // Extract type from context
            let release_type = type_re.captures(context).map(|c| strip_tags(&c[1]));

            // Extract group from context (try font version first, then plain)
            let group = group_re
                .captures(context)
                .map(|c| strip_tags(&c[1]))
                .or_else(|| group_alt_re.captures(context).map(|c| strip_tags(&c[1])));

            let release_url = format!("https://csdb.dk/release/?id={}", rid);

            results.push(SearchResult {
                release_id: Some(rid),
                title,
                release_url,
                group,
                release_type,
                year: None,
                exact_match: false,
            });
        }

        results
    }

    fn parse_latest_releases(&self, html: &str) -> Vec<LatestRelease> {
        let mut releases = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // CSDb homepage has latest releases in various sections
        // Look for release links: <a href="/release/?id=12345">Title</a>
        let release_re = Regex::new(r#"href="(/release/\?id=(\d+))"[^>]*>([^<]+)</a>"#).unwrap();

        for cap in release_re.captures_iter(html) {
            let rel_path = &cap[1];
            let rid = cap[2].to_string();
            let title = strip_tags(&cap[3]);

            // Skip if we've seen this release or if title looks like navigation
            if seen.contains(&rid) || title.len() < 2 || title.contains("...") {
                continue;
            }
            seen.insert(rid.clone());

            let release_url = format!("https://csdb.dk{}", rel_path);

            releases.push(LatestRelease {
                release_id: rid,
                title,
                release_url,
                group: None,
                release_type: None,
                date: None,
            });
        }

        releases
    }

    fn parse_top_list(&self, html: &str) -> Vec<TopListEntry> {
        let mut entries = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Top list format: <td><a href="/release/?id=249713">Nine</a> by <a href="/scener/?id=16473">Lft</a></td>
        // We need to find table rows with rank numbers

        // Pattern to match release link with optional "by author"
        let entry_re = Regex::new(
            r#"<td[^>]*><a href="/release/\?id=(\d+)"[^>]*>([^<]+)</a>(?:\s*by\s*<a href="/(?:scener|group)/\?id=\d+"[^>]*>([^<]+)</a>)?</td>"#
        ).unwrap();

        let mut rank = 0;
        for cap in entry_re.captures_iter(html) {
            let rid = cap[1].to_string();
            let title = strip_tags(&cap[2]);
            let author = cap.get(3).map(|m| strip_tags(m.as_str()));

            if seen.contains(&rid) {
                continue;
            }
            seen.insert(rid.clone());

            rank += 1;
            let release_url = format!("https://csdb.dk/release/?id={}", rid);

            entries.push(TopListEntry {
                rank,
                release_id: Some(rid),
                title,
                release_url,
                author,
            });
        }

        entries
    }

    async fn build_file_list(&self, html: &str, base_url: &str) -> Result<Vec<ReleaseFile>> {
        let candidates = parse_candidates(html, base_url);
        let mut items = Vec::new();
        let mut seen_filenames: HashSet<String> = HashSet::new();

        for (i, c) in candidates.into_iter().enumerate() {
            let index = i + 1;

            // If we have a target URL from the link text, use it
            let (final_url, filename) = if let Some(target) = &c.target_url {
                // Skip FTP URLs - we can't download those
                if target.starts_with("ftp://") {
                    continue;
                }
                (target.clone(), c.label.clone())
            } else if c.kind == "download" {
                // Need to resolve the download.php redirect
                match self.resolve_final_url(&c.url).await {
                    Ok(resolved) => {
                        let fname = label_from_url(&resolved);
                        (resolved, fname)
                    }
                    Err(_) => {
                        // Skip failed resolutions
                        continue;
                    }
                }
            } else {
                (c.url.clone(), c.label.clone())
            };

            // Skip duplicate filenames
            if seen_filenames.contains(&filename) {
                continue;
            }
            seen_filenames.insert(filename.clone());

            let ext = get_ext(&filename);

            items.push(ReleaseFile {
                index,
                kind: c.kind,
                id: c.id,
                url: c.url,
                final_url,
                filename,
                ext,
            });
        }

        // Re-index after filtering
        for (i, item) in items.iter_mut().enumerate() {
            item.index = i + 1;
        }

        Ok(items)
    }

    fn extract_title_from_html(&self, html: &str) -> Option<String> {
        // Look for <title>...</title> or specific release title patterns
        let title_re = Regex::new(r"<title>([^<]+)</title>").ok()?;
        if let Some(cap) = title_re.captures(html) {
            let title = strip_tags(&cap[1]);
            // CSDb titles often have " - CSDb" suffix
            let title = title.trim_end_matches(" - CSDb").to_string();
            if !title.is_empty() {
                return Some(title);
            }
        }
        None
    }

    fn extract_group_from_html(&self, html: &str) -> Option<String> {
        // Look for "Released by" section: <b>Released by :</b><br><a href="/scener/?id=...">Name</a>
        // or <a href="/group/?id=...">GroupName</a>
        let released_by_re = Regex::new(
            r#"<b>Released by\s*:?\s*</b>.*?<a href="/(?:scener|group)/\?id=\d+"[^>]*>([^<]+)</a>"#,
        )
        .ok()?;
        if let Some(cap) = released_by_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }

        // Fallback: look for any group link
        let group_re = Regex::new(r#"href="/group/\?id=\d+"[^>]*>([^<]+)</a>"#).ok()?;
        if let Some(cap) = group_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }
        None
    }

    fn extract_release_type_from_html(&self, html: &str) -> Option<String> {
        // Look for Type section: <b>Type :</b><br><a href="...">C64 One-File Demo</a>
        let type_re = Regex::new(r#"<b>Type\s*:?\s*</b>.*?<a href="[^"]*">([^<]+)</a>"#).ok()?;
        if let Some(cap) = type_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }

        // Fallback: plain text
        let type_plain_re = Regex::new(r"Type:\s*</td>\s*<td[^>]*>([^<]+)</td>").ok()?;
        if let Some(cap) = type_plain_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }
        None
    }

    fn extract_date_from_html(&self, html: &str) -> Option<String> {
        // Look for Release Date section: <b>Release Date :</b><br><font color="#99c2ff">2 February 2025</font>
        let date_re =
            Regex::new(r#"<b>Release Date\s*:?\s*</b>.*?<font[^>]*>([^<]+)</font>"#).ok()?;
        if let Some(cap) = date_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }

        // Fallback: plain text
        let date_plain_re = Regex::new(r"Release Date:\s*</td>\s*<td[^>]*>([^<]+)</td>").ok()?;
        if let Some(cap) = date_plain_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }
        None
    }

    fn extract_platform_from_html(&self, html: &str) -> Option<String> {
        // Look for platform info
        let platform_re = Regex::new(r"Platform:\s*</td>\s*<td[^>]*>([^<]+)</td>").ok()?;
        if let Some(cap) = platform_re.captures(html) {
            return Some(strip_tags(&cap[1]));
        }
        None
    }
}

impl Default for CsdbClient {
    fn default() -> Self {
        Self::new().expect("Failed to create CsdbClient")
    }
}

// -----------------------------------------------------------------------------
// ZIP extraction functions
// -----------------------------------------------------------------------------

/// Extract a ZIP file from bytes to a temporary directory
pub fn extract_zip(zip_data: &[u8], source_filename: &str) -> Result<ExtractedZip> {
    let reader = Cursor::new(zip_data);
    let mut archive = ZipArchive::new(reader).context("Failed to open ZIP archive")?;

    // Create temp directory for extraction with unique name
    let temp_dir = std::env::temp_dir().join(format!(
        "csdb_extract_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));

    std::fs::create_dir_all(&temp_dir).context("Failed to create temp directory")?;

    let mut files = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read ZIP entry")?;

        if file.is_dir() {
            continue;
        }

        let filename = file.name().to_string();

        // Skip directories, hidden files, and macOS metadata
        if filename.ends_with('/')
            || filename.starts_with('.')
            || filename.contains("/__MACOSX")
            || filename.contains("\\_MACOSX")
        {
            continue;
        }

        // Extract just the filename (strip path)
        let clean_filename = filename.rsplit('/').next().unwrap_or(&filename).to_string();
        if clean_filename.is_empty() || clean_filename.starts_with('.') {
            continue;
        }

        let out_path = temp_dir.join(&clean_filename);

        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .context("Failed to read ZIP entry contents")?;

        let size = contents.len() as u64;

        std::fs::write(&out_path, &contents)
            .with_context(|| format!("Failed to write {:?}", out_path))?;

        let ext = get_ext(&clean_filename);

        files.push(ExtractedFile {
            index: files.len() + 1,
            filename: clean_filename,
            path: out_path,
            ext,
            size,
        });
    }

    // Re-index to ensure sequential indices
    for (i, f) in files.iter_mut().enumerate() {
        f.index = i + 1;
    }

    Ok(ExtractedZip {
        source_filename: source_filename.to_string(),
        extract_dir: temp_dir,
        files,
    })
}

/// Get runnable files from extracted ZIP
pub fn get_runnable_extracted_files(files: &[ExtractedFile]) -> Vec<&ExtractedFile> {
    files
        .iter()
        .filter(|f| {
            matches!(
                f.ext.as_str(),
                "prg" | "d64" | "d71" | "d81" | "g64" | "crt" | "sid"
            )
        })
        .collect()
}

/// Check if a file extension indicates a ZIP archive
pub fn is_zip_file(ext: &str) -> bool {
    ext == "zip"
}

// -----------------------------------------------------------------------------
// Internal candidate structure for parsing
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Candidate {
    kind: String,
    url: String,
    id: Option<String>,
    label: String,
    target_url: Option<String>, // The actual file URL (from link text)
}

// -----------------------------------------------------------------------------
// Utility functions
// -----------------------------------------------------------------------------

fn strip_tags(s: &str) -> String {
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    let stripped = tag_re.replace_all(s, "");
    let decoded = html_escape::decode_html_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn label_from_url(url: &str) -> String {
    if let Ok(parsed) = Url::parse(url) {
        let path = parsed.path();
        // URL decode the path
        if let Ok(decoded) = urlencoding::decode(path) {
            if let Some(last) = decoded.rsplit('/').next() {
                if !last.is_empty() {
                    return last.to_string();
                }
            }
        } else if let Some(last) = path.rsplit('/').next() {
            if !last.is_empty() {
                return last.to_string();
            }
        }
    }
    url.to_string()
}

fn get_ext(filename: &str) -> String {
    if let Some(pos) = filename.rfind('.') {
        filename[pos + 1..].to_lowercase()
    } else {
        String::new()
    }
}

fn url_join(base: &str, relative: &str) -> String {
    if let Ok(base_url) = Url::parse(base) {
        if let Ok(joined) = base_url.join(relative) {
            return joined.to_string();
        }
    }
    format!("https://csdb.dk{}", relative)
}

fn parse_candidates(html: &str, base_url: &str) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();

    // Pattern for downloadLinks table entries:
    // <a href="download.php?id=140279">http://csdb.dk/getinternalfile.php/109865/coma-light-13-by-oxyron.zip</a>
    // The link TEXT contains the actual target URL!
    let download_table_re =
        Regex::new(r#"<a\s+href="((?:/release/)?download\.php\?id=(\d+))"[^>]*>([^<]+)</a>"#)
            .unwrap();

    for cap in download_table_re.captures_iter(html) {
        let href = &cap[1];
        let id = cap[2].to_string();
        let link_text = cap[3].trim();

        // The link text contains the actual target URL (http:// or ftp://)
        // Extract filename from it
        let (target_url, filename) =
            if link_text.starts_with("http") || link_text.starts_with("ftp") {
                let fname = label_from_url(link_text);
                (link_text.to_string(), fname)
            } else {
                (String::new(), format!("download-{}", id))
            };

        // Skip duplicates by target URL or filename
        let dedup_key = if !target_url.is_empty() {
            target_url.clone()
        } else {
            format!("download.php?id={}", id)
        };

        if seen_urls.contains(&dedup_key) {
            continue;
        }
        seen_urls.insert(dedup_key);

        let abs_url = if href.starts_with('/') {
            format!("https://csdb.dk{}", href)
        } else {
            url_join(base_url, href)
        };

        candidates.push(Candidate {
            kind: "download".to_string(),
            url: abs_url,
            id: Some(id),
            label: filename,
            target_url: if target_url.is_empty() {
                None
            } else {
                Some(target_url)
            },
        });
    }

    // Also look for direct getinternalfile.php links (absolute URLs in page)
    let internal_re =
        Regex::new(r#"https?://csdb\.dk/getinternalfile\.php/\d+/[^\s"'<>()]+"#).unwrap();
    for mat in internal_re.find_iter(html) {
        let abs_url = mat.as_str().to_string();

        if seen_urls.contains(&abs_url) {
            continue;
        }
        seen_urls.insert(abs_url.clone());

        candidates.push(Candidate {
            kind: "internal".to_string(),
            url: abs_url.clone(),
            id: None,
            label: label_from_url(&abs_url),
            target_url: Some(abs_url),
        });
    }

    candidates
}

/// Filter files by extension
pub fn filter_files_by_ext(files: &[ReleaseFile], extensions: &[&str]) -> Vec<ReleaseFile> {
    let ext_set: HashSet<&str> = extensions.iter().copied().collect();
    files
        .iter()
        .filter(|f| ext_set.contains(f.ext.as_str()))
        .cloned()
        .collect()
}

/// Get runnable files (PRG, D64, CRT, SID, etc.)
pub fn get_runnable_files(files: &[ReleaseFile]) -> Vec<ReleaseFile> {
    filter_files_by_ext(
        files,
        &["prg", "d64", "d71", "d81", "g64", "crt", "sid", "zip"],
    )
}
