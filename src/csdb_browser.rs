//! CSDb Browser UI component for Ultimate64 Manager
//!
//! Provides a UI for:
//! - Browsing latest releases from CSDb
//! - Searching for releases
//! - Viewing release details and files
//! - Downloading and running files on Ultimate64
//! - Extracting and browsing ZIP archives

use iced::{
    Task, Element, Length,
    widget::{
        Column, Space, button, column, container, pick_list,
        row, scrollable, text, text_input, tooltip, rule,
    },
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::csdb::{
    CsdbClient, ExtractedZip, LatestRelease, ReleaseDetails, ReleaseFile, SearchCategory,
    SearchResult, TopListCategory, TopListEntry, extract_zip, get_runnable_extracted_files,
    get_runnable_files, is_zip_file,
};

/// Timeout for CSDb operations
const CSDB_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub enum CsdbBrowserMessage {
    // Search
    SearchInputChanged(String),
    SearchCategoryChanged(SearchCategory),
    SearchSubmit,
    SearchCompleted(Result<Vec<SearchResult>, String>),

    // Latest releases
    RefreshLatest,
    LatestLoaded(Result<Vec<LatestRelease>, String>),

    // Top lists
    TopListCategoryChanged(TopListCategory),
    LoadTopList,
    TopListLoaded(Result<Vec<TopListEntry>, String>),

    // Release selection
    SelectRelease(String), // release URL
    ReleaseDetailsLoaded(Result<ReleaseDetails, String>),
    CloseReleaseDetails,

    // File operations
    SelectFile(usize), // file index
    DownloadFile(usize),
    DownloadCompleted(Result<PathBuf, String>),
    RunFile(usize),
    RunFileCompleted(Result<String, String>),

    // ZIP operations
    ExtractZip(usize), // file index of the ZIP file
    ZipExtracted(Result<ExtractedZip, String>),
    SelectExtractedFile(usize), // index within extracted files
    RunExtractedFile(usize),    // run file from extracted ZIP
    RunExtractedFileCompleted(Result<String, String>),
    MountExtractedFile(usize, MountMode), // mount disk image from extracted ZIP
    MountExtractedFileCompleted(Result<String, String>),
    CloseZipView,

    // Disk mounting
    DriveSelected(DriveOption),
    MountFile(usize, MountMode),
    MountCompleted(Result<String, String>),

    // Filter
    FilterChanged(FileFilter),

    // Navigation
    BackToList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveOption {
    A,
    B,
}

impl std::fmt::Display for DriveOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriveOption::A => write!(f, "Drive A (8)"),
            DriveOption::B => write!(f, "Drive B (9)"),
        }
    }
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
    Runnable, // PRG, D64, CRT, SID
    Disk,     // D64, D71, D81, G64
    Program,  // PRG, CRT
    Music,    // SID
    Archive,  // ZIP
}

impl std::fmt::Display for FileFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileFilter::All => write!(f, "All Files"),
            FileFilter::Runnable => write!(f, "Runnable"),
            FileFilter::Disk => write!(f, "Disk Images"),
            FileFilter::Program => write!(f, "Programs"),
            FileFilter::Music => write!(f, "Music (SID)"),
            FileFilter::Archive => write!(f, "Archives"),
        }
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
                matches!(
                    ext,
                    "prg" | "d64" | "d71" | "d81" | "g64" | "crt" | "sid" | "zip"
                )
            }
            FileFilter::Disk => matches!(ext, "d64" | "d71" | "d81" | "g64" | "g71"),
            FileFilter::Program => matches!(ext, "prg" | "crt"),
            FileFilter::Music => ext == "sid",
            FileFilter::Archive => ext == "zip",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewState {
    LatestReleases,
    SearchResults,
    TopList,
    ReleaseDetails,
    ZipContents, // New state for browsing ZIP contents
}

pub struct CsdbBrowser {
    // View state
    view_state: ViewState,

    // Search
    search_input: String,
    search_category: SearchCategory,
    search_results: Vec<SearchResult>,

    // Latest releases
    latest_releases: Vec<LatestRelease>,

    // Top lists
    top_list_category: TopListCategory,
    top_list_entries: Vec<TopListEntry>,

    // Current release details
    current_release: Option<ReleaseDetails>,
    selected_file_index: Option<usize>,

    // ZIP extraction
    extracted_zip: Option<ExtractedZip>,
    selected_extracted_file_index: Option<usize>,

    // Drive selection for mounting
    selected_drive: DriveOption,

    // Filter
    file_filter: FileFilter,

    // Status
    status_message: Option<String>,
    is_loading: bool,

    // Download directory
    download_dir: PathBuf,
}

impl CsdbBrowser {
    pub fn new() -> Self {
        let download_dir = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            view_state: ViewState::LatestReleases,
            search_input: String::new(),
            search_category: SearchCategory::default(),
            search_results: Vec::new(),
            latest_releases: Vec::new(),
            top_list_category: TopListCategory::default(),
            top_list_entries: Vec::new(),
            current_release: None,
            selected_file_index: None,
            extracted_zip: None,
            selected_extracted_file_index: None,
            selected_drive: DriveOption::A,
            file_filter: FileFilter::Runnable,
            status_message: None,
            is_loading: false,
            download_dir,
        }
    }

    pub fn update(
        &mut self,
        message: CsdbBrowserMessage,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<CsdbBrowserMessage> {
        match message {
            CsdbBrowserMessage::SearchInputChanged(value) => {
                self.search_input = value;
                Task::none()
            }

            CsdbBrowserMessage::SearchCategoryChanged(category) => {
                self.search_category = category;
                Task::none()
            }

            CsdbBrowserMessage::SearchSubmit => {
                if self.search_input.trim().is_empty() {
                    self.status_message = Some("Please enter a search term".to_string());
                    return Task::none();
                }

                self.is_loading = true;
                self.status_message = Some(format!("Searching for '{}'...", self.search_input));
                let term = self.search_input.clone();
                let category = self.search_category;

                Task::perform(
                    async move {
                        tokio::time::timeout(
                            tokio::time::Duration::from_secs(CSDB_TIMEOUT_SECS),
                            async {
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;
                                client
                                    .search(&term, category, 50)
                                    .await
                                    .map_err(|e| e.to_string())
                            },
                        )
                        .await
                        .map_err(|_| "Search timed out".to_string())?
                    },
                    CsdbBrowserMessage::SearchCompleted,
                )
            }

            CsdbBrowserMessage::SearchCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(results) => {
                        let count = results.len();
                        self.search_results = results;
                        self.view_state = ViewState::SearchResults;
                        self.status_message = Some(format!("Found {} release(s)", count));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Search failed: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::RefreshLatest => {
                self.is_loading = true;
                self.status_message = Some("Loading latest releases...".to_string());

                Task::perform(
                    async move {
                        tokio::time::timeout(
                            tokio::time::Duration::from_secs(CSDB_TIMEOUT_SECS),
                            async {
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;
                                client
                                    .get_latest_releases(50)
                                    .await
                                    .map_err(|e| e.to_string())
                            },
                        )
                        .await
                        .map_err(|_| "Loading timed out".to_string())?
                    },
                    CsdbBrowserMessage::LatestLoaded,
                )
            }

            CsdbBrowserMessage::LatestLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(releases) => {
                        let count = releases.len();
                        self.latest_releases = releases;
                        self.view_state = ViewState::LatestReleases;
                        self.status_message = Some(format!("Loaded {} release(s)", count));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to load: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::TopListCategoryChanged(category) => {
                self.top_list_category = category;
                Task::none()
            }

            CsdbBrowserMessage::LoadTopList => {
                self.is_loading = true;
                self.status_message =
                    Some(format!("Loading {} top list...", self.top_list_category));
                let category = self.top_list_category;

                Task::perform(
                    async move {
                        tokio::time::timeout(
                            tokio::time::Duration::from_secs(CSDB_TIMEOUT_SECS),
                            async {
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;
                                client
                                    .get_top_list(category, 100)
                                    .await
                                    .map_err(|e| e.to_string())
                            },
                        )
                        .await
                        .map_err(|_| "Loading timed out".to_string())?
                    },
                    CsdbBrowserMessage::TopListLoaded,
                )
            }

            CsdbBrowserMessage::TopListLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(entries) => {
                        let count = entries.len();
                        self.top_list_entries = entries;
                        self.view_state = ViewState::TopList;
                        self.status_message = Some(format!("Loaded top {} entries", count));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to load top list: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::SelectRelease(release_url) => {
                self.is_loading = true;
                self.status_message = Some("Loading release details...".to_string());

                Task::perform(
                    async move {
                        tokio::time::timeout(
                            tokio::time::Duration::from_secs(CSDB_TIMEOUT_SECS),
                            async {
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;
                                client
                                    .get_release_details(&release_url)
                                    .await
                                    .map_err(|e| e.to_string())
                            },
                        )
                        .await
                        .map_err(|_| "Loading timed out".to_string())?
                    },
                    CsdbBrowserMessage::ReleaseDetailsLoaded,
                )
            }

            CsdbBrowserMessage::ReleaseDetailsLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(details) => {
                        let file_count = details.files.len();
                        let runnable_count = get_runnable_files(&details.files).len();
                        self.current_release = Some(details);
                        self.view_state = ViewState::ReleaseDetails;
                        self.selected_file_index = None;
                        self.status_message = Some(format!(
                            "{} file(s), {} runnable",
                            file_count, runnable_count
                        ));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to load release: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::CloseReleaseDetails => {
                self.current_release = None;
                self.selected_file_index = None;
                self.extracted_zip = None;
                self.selected_extracted_file_index = None;
                // Go back to previous view
                if !self.search_results.is_empty() {
                    self.view_state = ViewState::SearchResults;
                } else {
                    self.view_state = ViewState::LatestReleases;
                }
                Task::none()
            }

            CsdbBrowserMessage::SelectFile(index) => {
                self.selected_file_index = Some(index);
                Task::none()
            }

            CsdbBrowserMessage::DownloadFile(index) => {
                if let Some(release) = &self.current_release {
                    if let Some(file) = release.files.iter().find(|f| f.index == index) {
                        self.is_loading = true;
                        self.status_message = Some(format!("Downloading {}...", file.filename));

                        let file = file.clone();
                        let out_dir = self.download_dir.clone();

                        return Task::perform(
                            async move {
                                tokio::time::timeout(
                                    tokio::time::Duration::from_secs(120), // 2 minutes for large files
                                    async {
                                        let client =
                                            CsdbClient::new().map_err(|e| e.to_string())?;
                                        client
                                            .download_file(&file, &out_dir)
                                            .await
                                            .map_err(|e| e.to_string())
                                    },
                                )
                                .await
                                .map_err(|_| "Download timed out".to_string())?
                            },
                            CsdbBrowserMessage::DownloadCompleted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::DownloadCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(path) => {
                        self.status_message = Some(format!("Downloaded: {}", path.display()));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Download failed: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::RunFile(index) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    return Task::none();
                }

                if let Some(release) = &self.current_release {
                    if let Some(file) = release.files.iter().find(|f| f.index == index) {
                        self.is_loading = true;
                        self.status_message = Some(format!("Running {}...", file.filename));

                        let file = file.clone();
                        let conn = connection.unwrap();
                        let drive = self.selected_drive.to_drive_string();
                        let device_num = self.selected_drive.device_number().to_string();
                        let is_disk = matches!(file.ext.as_str(), "d64" | "d71" | "d81" | "g64");

                        return Task::perform(
                            async move {
                                // First download the file (separate timeout)
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;

                                let (filename, data) = tokio::time::timeout(
                                    tokio::time::Duration::from_secs(60),
                                    client.download_file_bytes(&file),
                                )
                                .await
                                .map_err(|_| "Download timed out".to_string())?
                                .map_err(|e| e.to_string())?;

                                // Determine file type and run
                                let ext = file.ext.to_lowercase();

                                // Use longer timeout for disk images (includes boot + load delays)
                                let run_timeout = if is_disk { 30 } else { 15 };

                                tokio::time::timeout(
                                    tokio::time::Duration::from_secs(run_timeout),
                                    tokio::task::spawn_blocking(move || {
                                        let conn = conn.blocking_lock();

                                        match ext.as_str() {
                                            "prg" => conn
                                                .run_prg(&data)
                                                .map(|_| format!("Running: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "crt" => conn
                                                .run_crt(&data)
                                                .map(|_| format!("Running cartridge: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "sid" => conn
                                                .sid_play(&data, None)
                                                .map(|_| format!("Playing: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "d64" | "d71" | "d81" | "g64" => {
                                                // For disk images, we need to save to temp and mount
                                                let temp_dir = std::env::temp_dir();
                                                let temp_path = temp_dir.join(&filename);
                                                std::fs::write(&temp_path, &data).map_err(|e| {
                                                    format!("Failed to write temp file: {}", e)
                                                })?;

                                                // Mount and run
                                                conn.mount_disk_image(
                                                    &temp_path,
                                                    drive,
                                                    ultimate64::drives::MountMode::ReadOnly,
                                                    false,
                                                )
                                                .map_err(|e| format!("Mount failed: {}", e))?;

                                                std::thread::sleep(
                                                    std::time::Duration::from_millis(500),
                                                );
                                                conn.reset()
                                                    .map_err(|e| format!("Reset failed: {}", e))?;
                                                std::thread::sleep(std::time::Duration::from_secs(
                                                    3,
                                                ));
                                                conn.type_text(&format!(
                                                    "load \"*\",{},1\n",
                                                    device_num
                                                ))
                                                .map_err(|e| format!("Type failed: {}", e))?;
                                                std::thread::sleep(std::time::Duration::from_secs(
                                                    5,
                                                ));
                                                conn.type_text("run\n")
                                                    .map_err(|e| format!("Type failed: {}", e))?;

                                                Ok(format!("Running disk: {}", filename))
                                            }
                                            _ => Err(format!("Unsupported file type: {}", ext)),
                                        }
                                    }),
                                )
                                .await
                                .map_err(|_| {
                                    "Run timed out - device may be busy or offline".to_string()
                                })?
                                .map_err(|e| format!("Task error: {}", e))?
                            },
                            CsdbBrowserMessage::RunFileCompleted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::RunFileCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Run failed: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::DriveSelected(drive) => {
                self.selected_drive = drive;
                Task::none()
            }

            CsdbBrowserMessage::MountFile(index, mount_mode) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    return Task::none();
                }

                if let Some(release) = &self.current_release {
                    if let Some(file) = release.files.iter().find(|f| f.index == index) {
                        // Only allow mounting disk images
                        if !matches!(file.ext.as_str(), "d64" | "d71" | "d81" | "g64") {
                            self.status_message =
                                Some("Only disk images can be mounted".to_string());
                            return Task::none();
                        }

                        self.is_loading = true;
                        let drive_str = self.selected_drive.to_drive_string();
                        self.status_message = Some(format!(
                            "Mounting {} to Drive {}...",
                            file.filename,
                            self.selected_drive.device_number()
                        ));

                        let file = file.clone();
                        let conn = connection.unwrap();
                        let drive = drive_str;
                        let mode = match mount_mode {
                            MountMode::ReadOnly => ultimate64::drives::MountMode::ReadOnly,
                            MountMode::ReadWrite => ultimate64::drives::MountMode::ReadWrite,
                        };

                        return Task::perform(
                            async move {
                                // First download the file (separate timeout)
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;

                                let (filename, data) = tokio::time::timeout(
                                    tokio::time::Duration::from_secs(60),
                                    client.download_file_bytes(&file),
                                )
                                .await
                                .map_err(|_| "Download timed out".to_string())?
                                .map_err(|e| e.to_string())?;

                                // Save to temp
                                let temp_dir = std::env::temp_dir();
                                let temp_path = temp_dir.join(&filename);

                                tokio::fs::write(&temp_path, &data)
                                    .await
                                    .map_err(|e| format!("Failed to write temp file: {}", e))?;

                                // Mount (separate timeout)
                                tokio::time::timeout(
                                    tokio::time::Duration::from_secs(15),
                                    tokio::task::spawn_blocking(move || {
                                        let conn = conn.blocking_lock();
                                        conn.mount_disk_image(&temp_path, drive, mode, false)
                                            .map(|_| format!("Mounted: {}", filename))
                                            .map_err(|e| format!("Mount failed: {}", e))
                                    }),
                                )
                                .await
                                .map_err(|_| {
                                    "Mount timed out - device may be busy or offline".to_string()
                                })?
                                .map_err(|e| format!("Task error: {}", e))?
                            },
                            CsdbBrowserMessage::MountCompleted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::MountCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Mount failed: {}", e));
                    }
                }
                Task::none()
            }

            // ZIP extraction messages
            CsdbBrowserMessage::ExtractZip(index) => {
                if let Some(release) = &self.current_release {
                    if let Some(file) = release.files.iter().find(|f| f.index == index) {
                        if !is_zip_file(&file.ext) {
                            self.status_message = Some("Not a ZIP file".to_string());
                            return Task::none();
                        }

                        self.is_loading = true;
                        self.status_message = Some(format!("Extracting {}...", file.filename));

                        let file = file.clone();

                        return Task::perform(
                            async move {
                                // Download the ZIP file
                                let client = CsdbClient::new().map_err(|e| e.to_string())?;

                                let (filename, data) = tokio::time::timeout(
                                    tokio::time::Duration::from_secs(120),
                                    client.download_file_bytes(&file),
                                )
                                .await
                                .map_err(|_| "Download timed out".to_string())?
                                .map_err(|e| e.to_string())?;

                                // Extract the ZIP (blocking operation)
                                tokio::task::spawn_blocking(move || {
                                    extract_zip(&data, &filename).map_err(|e| e.to_string())
                                })
                                .await
                                .map_err(|e| format!("Task error: {}", e))?
                            },
                            CsdbBrowserMessage::ZipExtracted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::ZipExtracted(result) => {
                self.is_loading = false;
                match result {
                    Ok(extracted) => {
                        let file_count = extracted.files.len();
                        let runnable_count = get_runnable_extracted_files(&extracted.files).len();
                        self.status_message = Some(format!(
                            "Extracted {} file(s), {} runnable",
                            file_count, runnable_count
                        ));
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

            CsdbBrowserMessage::SelectExtractedFile(index) => {
                self.selected_extracted_file_index = Some(index);
                Task::none()
            }

            CsdbBrowserMessage::RunExtractedFile(index) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    return Task::none();
                }

                if let Some(extracted) = &self.extracted_zip {
                    if let Some(file) = extracted.files.iter().find(|f| f.index == index) {
                        self.is_loading = true;
                        self.status_message = Some(format!("Running {}...", file.filename));

                        let file_path = file.path.clone();
                        let filename = file.filename.clone();
                        let ext = file.ext.clone();
                        let conn = connection.unwrap();
                        let drive = self.selected_drive.to_drive_string();
                        let device_num = self.selected_drive.device_number().to_string();
                        let is_disk = matches!(ext.as_str(), "d64" | "d71" | "d81" | "g64");

                        return Task::perform(
                            async move {
                                // Read file from extracted location
                                let data = tokio::fs::read(&file_path)
                                    .await
                                    .map_err(|e| format!("Failed to read file: {}", e))?;

                                // Use longer timeout for disk images
                                let run_timeout = if is_disk { 30 } else { 15 };

                                tokio::time::timeout(
                                    tokio::time::Duration::from_secs(run_timeout),
                                    tokio::task::spawn_blocking(move || {
                                        let conn = conn.blocking_lock();

                                        match ext.as_str() {
                                            "prg" => conn
                                                .run_prg(&data)
                                                .map(|_| format!("Running: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "crt" => conn
                                                .run_crt(&data)
                                                .map(|_| format!("Running cartridge: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "sid" => conn
                                                .sid_play(&data, None)
                                                .map(|_| format!("Playing: {}", filename))
                                                .map_err(|e| e.to_string()),
                                            "d64" | "d71" | "d81" | "g64" => {
                                                // Mount and run
                                                conn.mount_disk_image(
                                                    &file_path,
                                                    drive,
                                                    ultimate64::drives::MountMode::ReadOnly,
                                                    false,
                                                )
                                                .map_err(|e| format!("Mount failed: {}", e))?;

                                                std::thread::sleep(
                                                    std::time::Duration::from_millis(500),
                                                );
                                                conn.reset()
                                                    .map_err(|e| format!("Reset failed: {}", e))?;
                                                std::thread::sleep(std::time::Duration::from_secs(
                                                    3,
                                                ));
                                                conn.type_text(&format!(
                                                    "load \"*\",{},1\n",
                                                    device_num
                                                ))
                                                .map_err(|e| format!("Type failed: {}", e))?;
                                                std::thread::sleep(std::time::Duration::from_secs(
                                                    5,
                                                ));
                                                conn.type_text("run\n")
                                                    .map_err(|e| format!("Type failed: {}", e))?;

                                                Ok(format!("Running disk: {}", filename))
                                            }
                                            _ => Err(format!("Unsupported file type: {}", ext)),
                                        }
                                    }),
                                )
                                .await
                                .map_err(|_| {
                                    "Run timed out - device may be busy or offline".to_string()
                                })?
                                .map_err(|e| format!("Task error: {}", e))?
                            },
                            CsdbBrowserMessage::RunExtractedFileCompleted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::RunExtractedFileCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Run failed: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::MountExtractedFile(index, mount_mode) => {
                if connection.is_none() {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    return Task::none();
                }

                if let Some(extracted) = &self.extracted_zip {
                    if let Some(file) = extracted.files.iter().find(|f| f.index == index) {
                        // Only allow mounting disk images
                        if !matches!(file.ext.as_str(), "d64" | "d71" | "d81" | "g64") {
                            self.status_message =
                                Some("Only disk images can be mounted".to_string());
                            return Task::none();
                        }

                        self.is_loading = true;
                        let drive_str = self.selected_drive.to_drive_string();
                        self.status_message = Some(format!(
                            "Mounting {} to Drive {}...",
                            file.filename,
                            self.selected_drive.device_number()
                        ));

                        let file_path = file.path.clone();
                        let filename = file.filename.clone();
                        let conn = connection.unwrap();
                        let drive = drive_str;
                        let mode = match mount_mode {
                            MountMode::ReadOnly => ultimate64::drives::MountMode::ReadOnly,
                            MountMode::ReadWrite => ultimate64::drives::MountMode::ReadWrite,
                        };

                        return Task::perform(
                            async move {
                                tokio::time::timeout(
                                    tokio::time::Duration::from_secs(15),
                                    tokio::task::spawn_blocking(move || {
                                        let conn = conn.blocking_lock();
                                        conn.mount_disk_image(&file_path, drive, mode, false)
                                            .map(|_| format!("Mounted: {}", filename))
                                            .map_err(|e| format!("Mount failed: {}", e))
                                    }),
                                )
                                .await
                                .map_err(|_| {
                                    "Mount timed out - device may be busy or offline".to_string()
                                })?
                                .map_err(|e| format!("Task error: {}", e))?
                            },
                            CsdbBrowserMessage::MountExtractedFileCompleted,
                        );
                    }
                }
                self.status_message = Some("No file selected".to_string());
                Task::none()
            }

            CsdbBrowserMessage::MountExtractedFileCompleted(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Mount failed: {}", e));
                    }
                }
                Task::none()
            }

            CsdbBrowserMessage::CloseZipView => {
                self.extracted_zip = None;
                self.selected_extracted_file_index = None;
                self.view_state = ViewState::ReleaseDetails;
                Task::none()
            }

            CsdbBrowserMessage::FilterChanged(filter) => {
                self.file_filter = filter;
                Task::none()
            }

            CsdbBrowserMessage::BackToList => {
                self.current_release = None;
                self.selected_file_index = None;
                self.extracted_zip = None;
                self.selected_extracted_file_index = None;
                if !self.search_results.is_empty() {
                    self.view_state = ViewState::SearchResults;
                } else if !self.top_list_entries.is_empty() {
                    self.view_state = ViewState::TopList;
                } else {
                    self.view_state = ViewState::LatestReleases;
                }
                Task::none()
            }
        }
    }

    pub fn view(&self, font_size: u32, is_connected: bool) -> Element<'_, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;

        // Search bar at top
        let search_bar = row![
            text_input("Search CSDb...", &self.search_input)
                .on_input(CsdbBrowserMessage::SearchInputChanged)
                .on_submit(CsdbBrowserMessage::SearchSubmit)
                .padding(8)
                .size(normal)
                .width(Length::FillPortion(3)),
            pick_list(
                SearchCategory::all_categories(),
                Some(self.search_category),
                CsdbBrowserMessage::SearchCategoryChanged,
            )
            .text_size(normal)
            .width(Length::Fixed(90.0)),
            tooltip(
                button(text("Search").size(normal))
                    .on_press(CsdbBrowserMessage::SearchSubmit)
                    .padding([8, 12]),
                "Search CSDb",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            Space::new().width(15),
            tooltip(
                button(text("Latest").size(normal))
                    .on_press(CsdbBrowserMessage::RefreshLatest)
                    .padding([8, 12]),
                "Load latest releases",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            Space::new().width(15),
            pick_list(
                TopListCategory::all_categories(),
                Some(self.top_list_category),
                CsdbBrowserMessage::TopListCategoryChanged,
            )
            .text_size(small)
            .width(Length::Fixed(150.0)),
            tooltip(
                button(text("Top List").size(normal))
                    .on_press(CsdbBrowserMessage::LoadTopList)
                    .padding([8, 12]),
                "Load top rated releases",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Main content based on view state
        let content: Element<'_, CsdbBrowserMessage> = match &self.view_state {
            ViewState::LatestReleases => {
                self.view_releases_list(&self.latest_releases, "Latest Releases", font_size)
            }
            ViewState::SearchResults => self.view_search_results(font_size),
            ViewState::TopList => self.view_top_list(font_size),
            ViewState::ReleaseDetails => self.view_release_details(font_size, is_connected),
            ViewState::ZipContents => self.view_zip_contents(font_size, is_connected),
        };

        // Status bar
        let status = if self.is_loading {
            text(self.status_message.as_deref().unwrap_or("Loading...")).size(small)
        } else if let Some(msg) = &self.status_message {
            text(msg).size(small)
        } else {
            text("Ready").size(small)
        };

        let connection_status = if is_connected {
            text("● Connected")
                .size(small)
                .color(iced::Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            text("○ Not connected")
                .size(small)
                .color(iced::Color::from_rgb(0.8, 0.5, 0.2))
        };

        let status_bar = row![status, Space::new().width(Length::Fill), connection_status,]
            .spacing(10)
            .align_y(iced::Alignment::Center);

        column![
            search_bar,
            rule::horizontal(1),
            content,
            rule::horizontal(1),
            status_bar,
        ]
        .spacing(5)
        .padding(5)
        .into()
    }

    fn view_releases_list<'a>(
        &'a self,
        releases: &'a [impl ReleaseItem],
        title: &'a str,
        font_size: u32,
    ) -> Element<'a, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;

        if releases.is_empty() {
            return container(
                column![
                    text(title).size(normal + 2),
                    Space::new().height(20),
                    text("No releases loaded. Click 'Latest' to load recent releases.")
                        .size(normal),
                ]
                .spacing(10)
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .padding(20)
            .into();
        }

        let header = row![
            text(title).size(normal + 2),
            Space::new().width(Length::Fill),
            text(format!("{} release(s)", releases.len())).size(small),
        ]
        .align_y(iced::Alignment::Center);

        let mut items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();
        for release in releases {
            items.push(self.view_release_item(release, font_size));
            items.push(rule::horizontal(1).into());
        }

        let list = scrollable(
            Column::with_children(items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![header, rule::horizontal(1), list,].spacing(5).into()
    }

    fn view_search_results(&self, font_size: u32) -> Element<'_, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;

        if self.search_results.is_empty() {
            return container(
                column![
                    text("Search Results").size(normal + 2),
                    Space::new().height(20),
                    text("No results found. Try a different search term.").size(normal),
                ]
                .spacing(10)
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .padding(20)
            .into();
        }

        let header = row![
            text(format!("Search Results: '{}'", self.search_input)).size(normal + 2),
            Space::new().width(Length::Fill),
            text(format!("{} result(s)", self.search_results.len())).size(small),
        ]
        .align_y(iced::Alignment::Center);

        let mut items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();
        for result in &self.search_results {
            items.push(self.view_release_item(result, font_size));
            items.push(rule::horizontal(1).into());
        }

        let list = scrollable(
            Column::with_children(items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![header, rule::horizontal(1), list,].spacing(5).into()
    }

    fn view_top_list(&self, font_size: u32) -> Element<'_, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        if self.top_list_entries.is_empty() {
            return container(
                column![
                    text("Top List").size(normal + 2),
                    Space::new().height(20),
                    text("No entries loaded. Select a category and click 'Top List'.").size(normal),
                ]
                .spacing(10)
                .align_x(iced::Alignment::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .padding(20)
            .into();
        }

        let header = row![
            text(format!("Top List: {}", self.top_list_category)).size(normal + 2),
            Space::new().width(Length::Fill),
            text(format!("{} entries", self.top_list_entries.len())).size(small),
        ]
        .align_y(iced::Alignment::Center);

        let mut items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();
        for entry in &self.top_list_entries {
            let title = &entry.title;
            let url = &entry.release_url;
            let rank = entry.rank;

            let title_display = if title.len() > 40 {
                format!("{}...", &title[..37])
            } else {
                title.to_string()
            };

            // Rank color: gold for top 3, silver-ish for top 10, normal for others
            let rank_color = if rank <= 3 {
                iced::Color::from_rgb(1.0, 0.84, 0.0) // Gold
            } else if rank <= 10 {
                iced::Color::from_rgb(0.75, 0.75, 0.8) // Silver
            } else {
                iced::Color::from_rgb(0.6, 0.6, 0.6)
            };

            let mut entry_row = row![
                text(format!("#{:<3}", rank))
                    .size(small)
                    .width(Length::Fixed(45.0))
                    .color(rank_color),
                tooltip(
                    button(text(title_display.clone()).size(normal))
                        .on_press(CsdbBrowserMessage::SelectRelease(url.to_string()))
                        .padding([4, 8])
                        .width(Length::Fill)
                        .style(button::text),
                    text(title).size(normal),
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center);

            // Show author if available
            if let Some(author) = &entry.author {
                entry_row = entry_row.push(
                    text(format!("by {}", author))
                        .size(tiny)
                        .width(Length::Fixed(120.0))
                        .color(iced::Color::from_rgb(0.6, 0.7, 0.8)),
                );
            }

            entry_row = entry_row.push(
                tooltip(
                    button(text("View").size(small))
                        .on_press(CsdbBrowserMessage::SelectRelease(url.to_string()))
                        .padding([4, 10]),
                    "View release details",
                    tooltip::Position::Left,
                )
                .style(container::bordered_box),
            );

            items.push(entry_row.padding([4, 0]).into());
            items.push(rule::horizontal(1).into());
        }

        let list = scrollable(
            Column::with_children(items)
                .spacing(0)
                .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill);

        column![header, rule::horizontal(1), list,].spacing(5).into()
    }

    fn view_release_item<'a>(
        &'a self,
        release: &'a impl ReleaseItem,
        font_size: u32,
    ) -> Element<'a, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        let title = release.title();
        let url = release.url();
        let id = release.id().unwrap_or_default();
        let rtype = release.release_type();
        let group = release.group();

        let title_display: String = if title.len() > 30 {
            format!("{}...", &title[..27])
        } else {
            title.to_string()
        };

        // Type column
        let type_display: String = rtype
            .map(|t| {
                if t.len() > 16 {
                    format!("{}...", &t[..13])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_default();

        // Group column
        let group_display: String = group
            .map(|g| {
                if g.len() > 18 {
                    format!("{}...", &g[..15])
                } else {
                    g.to_string()
                }
            })
            .unwrap_or_default();

        row![
            text(format!("[{}]", id))
                .size(tiny)
                .width(Length::Fixed(70.0))
                .color(iced::Color::from_rgb(0.5, 0.5, 0.6)),
            tooltip(
                button(text(title_display).size(normal))
                    .on_press(CsdbBrowserMessage::SelectRelease(url.to_string()))
                    .padding([4, 8])
                    .width(Length::Fixed(250.0))
                    .style(button::text),
                text(title.to_string()).size(normal),
                tooltip::Position::Top,
            )
            .style(container::bordered_box),
            text(type_display)
                .size(tiny)
                .width(Length::Fixed(130.0))
                .color(iced::Color::from_rgb(0.6, 0.8, 0.6)),
            text(group_display)
                .size(tiny)
                .width(Length::Fill)
                .color(iced::Color::from_rgb(0.5, 0.7, 0.9)),
            tooltip(
                button(text("View").size(small))
                    .on_press(CsdbBrowserMessage::SelectRelease(url.to_string()))
                    .padding([4, 10]),
                "View release details and files",
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center)
        .padding([4, 0])
        .into()
    }

    fn view_release_details(
        &self,
        font_size: u32,
        is_connected: bool,
    ) -> Element<'_, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        let release = match &self.current_release {
            Some(r) => r,
            None => {
                return container(text("No release selected").size(normal))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into();
            }
        };

        // Header with back button and title
        let header = row![
            tooltip(
                button(text("← Back").size(normal))
                    .on_press(CsdbBrowserMessage::BackToList)
                    .padding([6, 12]),
                "Back to list",
                tooltip::Position::Right,
            )
            .style(container::bordered_box),
            Space::new().width(10),
            text(&release.title).size(normal + 2),
            Space::new().width(Length::Fill),
            text(format!("ID: {}", release.release_id)).size(small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Release info
        let mut info_items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();

        if let Some(group) = &release.group {
            info_items.push(text(format!("Group: {}", group)).size(small).into());
        }
        if let Some(rtype) = &release.release_type {
            info_items.push(text(format!("Type: {}", rtype)).size(small).into());
        }
        if let Some(date) = &release.release_date {
            info_items.push(text(format!("Date: {}", date)).size(small).into());
        }
        if let Some(platform) = &release.platform {
            info_items.push(text(format!("Platform: {}", platform)).size(small).into());
        }

        let info_row = row(info_items).spacing(20);

        // Filter and drive selector
        let filter_row = row![
            text("Filter:").size(small),
            pick_list(
                FileFilter::all(),
                Some(self.file_filter),
                CsdbBrowserMessage::FilterChanged,
            )
            .text_size(normal)
            .width(Length::Fixed(130.0)),
            Space::new().width(20),
            text("Mount to:").size(small),
            pick_list(
                DriveOption::all(),
                Some(self.selected_drive),
                CsdbBrowserMessage::DriveSelected,
            )
            .text_size(normal)
            .width(Length::Fixed(110.0)),
            Space::new().width(Length::Fill),
            text(format!("{} file(s)", release.files.len())).size(small),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // File list
        let filtered_files: Vec<&ReleaseFile> = release
            .files
            .iter()
            .filter(|f| self.file_filter.matches(&f.ext))
            .collect();

        let mut file_items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();

        if filtered_files.is_empty() {
            file_items.push(
                container(text("No files match the current filter").size(normal))
                    .padding(20)
                    .into(),
            );
        } else {
            for file in filtered_files {
                let is_selected = self.selected_file_index == Some(file.index);
                let is_runnable = matches!(
                    file.ext.as_str(),
                    "prg" | "crt" | "sid" | "d64" | "d71" | "d81" | "g64"
                );
                let is_disk_image = matches!(file.ext.as_str(), "d64" | "d71" | "d81" | "g64");
                let is_zip = is_zip_file(&file.ext);

                let filename_display = if file.filename.len() > 35 {
                    format!("{}...", &file.filename[..32])
                } else {
                    file.filename.clone()
                };

                let ext_color = match file.ext.as_str() {
                    "prg" => iced::Color::from_rgb(0.5, 0.8, 0.5),
                    "d64" | "d71" | "d81" | "g64" => iced::Color::from_rgb(0.5, 0.7, 0.9),
                    "crt" => iced::Color::from_rgb(0.9, 0.7, 0.5),
                    "sid" => iced::Color::from_rgb(0.8, 0.5, 0.8),
                    "zip" => iced::Color::from_rgb(0.9, 0.9, 0.5),
                    _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
                };

                let mut file_row = row![
                    text(format!("{:02}.", file.index))
                        .size(tiny)
                        .width(Length::Fixed(30.0)),
                    tooltip(
                        button(text(filename_display.clone()).size(normal))
                            .on_press(CsdbBrowserMessage::SelectFile(file.index))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(if is_selected {
                                button::primary
                            } else {
                                button::text
                            }),
                        text(&file.filename).size(normal),
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                    text(file.ext.to_uppercase())
                        .size(tiny)
                        .width(Length::Fixed(40.0))
                        .color(ext_color),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                // Download button (always available)
                file_row = file_row.push(
                    tooltip(
                        button(text("↓").size(small))
                            .on_press(CsdbBrowserMessage::DownloadFile(file.index))
                            .padding([4, 8]),
                        "Download file",
                        tooltip::Position::Left,
                    )
                    .style(container::bordered_box),
                );

                // Extract button for ZIP files
                if is_zip {
                    file_row = file_row.push(
                        tooltip(
                            button(text("📦").size(small))
                                .on_press(CsdbBrowserMessage::ExtractZip(file.index))
                                .padding([4, 8]),
                            "Extract ZIP and browse contents",
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );
                }

                // Mount buttons for disk images (when connected)
                if is_disk_image && is_connected {
                    let drive_label = self.selected_drive.device_number();

                    file_row = file_row.push(
                        tooltip(
                            button(text(format!("{}:RO", drive_label)).size(tiny))
                                .on_press(CsdbBrowserMessage::MountFile(
                                    file.index,
                                    MountMode::ReadOnly,
                                ))
                                .padding([4, 6]),
                            text(format!("Mount as Drive {} (Read Only)", drive_label))
                                .size(normal),
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );

                    file_row = file_row.push(
                        tooltip(
                            button(text(format!("{}:RW", drive_label)).size(tiny))
                                .on_press(CsdbBrowserMessage::MountFile(
                                    file.index,
                                    MountMode::ReadWrite,
                                ))
                                .padding([4, 6]),
                            text(format!("Mount as Drive {} (Read/Write)", drive_label))
                                .size(normal),
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );
                }

                // Run button for runnable files (when connected)
                if is_runnable && is_connected {
                    file_row = file_row.push(
                        tooltip(
                            button(text("▶").size(small))
                                .on_press(CsdbBrowserMessage::RunFile(file.index))
                                .padding([4, 8]),
                            if is_disk_image {
                                "Mount, reset, and run (LOAD\"*\",8,1 + RUN)"
                            } else {
                                "Run on Ultimate64"
                            },
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );
                }

                file_items.push(file_row.padding([2, 0]).into());
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
            info_row,
            rule::horizontal(1),
            filter_row,
            rule::horizontal(1),
            file_list,
        ]
        .spacing(5)
        .into()
    }

    /// View for browsing extracted ZIP contents
    fn view_zip_contents(
        &self,
        font_size: u32,
        is_connected: bool,
    ) -> Element<'_, CsdbBrowserMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let tiny = (font_size.saturating_sub(3)).max(7);

        let extracted = match &self.extracted_zip {
            Some(e) => e,
            None => {
                return container(text("No ZIP extracted").size(normal))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into();
            }
        };

        // Header with back button and ZIP filename
        let header = row![
            tooltip(
                button(text("← Back").size(normal))
                    .on_press(CsdbBrowserMessage::CloseZipView)
                    .padding([6, 12]),
                "Back to release",
                tooltip::Position::Right,
            )
            .style(container::bordered_box),
            Space::new().width(10),
            text(format!("📦 {}", extracted.source_filename)).size(normal + 2),
            Space::new().width(Length::Fill),
            text(format!("{} file(s)", extracted.files.len())).size(small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Drive selector row
        let drive_row = row![
            text("Mount to:").size(small),
            pick_list(
                DriveOption::all(),
                Some(self.selected_drive),
                CsdbBrowserMessage::DriveSelected,
            )
            .text_size(normal)
            .width(Length::Fixed(110.0)),
            Space::new().width(Length::Fill),
            text(format!("Extracted to: {}", extracted.extract_dir.display()))
                .size(tiny)
                .color(iced::Color::from_rgb(0.5, 0.5, 0.6)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // File list
        let mut file_items: Vec<Element<'_, CsdbBrowserMessage>> = Vec::new();

        if extracted.files.is_empty() {
            file_items.push(
                container(text("No files in archive").size(normal))
                    .padding(20)
                    .into(),
            );
        } else {
            for file in &extracted.files {
                let is_selected = self.selected_extracted_file_index == Some(file.index);
                let is_runnable = matches!(
                    file.ext.as_str(),
                    "prg" | "crt" | "sid" | "d64" | "d71" | "d81" | "g64"
                );
                let is_disk_image = matches!(file.ext.as_str(), "d64" | "d71" | "d81" | "g64");

                let filename_display = if file.filename.len() > 40 {
                    format!("{}...", &file.filename[..37])
                } else {
                    file.filename.clone()
                };

                let ext_color = match file.ext.as_str() {
                    "prg" => iced::Color::from_rgb(0.5, 0.8, 0.5),
                    "d64" | "d71" | "d81" | "g64" => iced::Color::from_rgb(0.5, 0.7, 0.9),
                    "crt" => iced::Color::from_rgb(0.9, 0.7, 0.5),
                    "sid" => iced::Color::from_rgb(0.8, 0.5, 0.8),
                    _ => iced::Color::from_rgb(0.6, 0.6, 0.6),
                };

                // Format file size
                let size_str = if file.size >= 1024 * 1024 {
                    format!("{:.1} MB", file.size as f64 / (1024.0 * 1024.0))
                } else if file.size >= 1024 {
                    format!("{:.1} KB", file.size as f64 / 1024.0)
                } else {
                    format!("{} B", file.size)
                };

                let mut file_row = row![
                    text(format!("{:02}.", file.index))
                        .size(tiny)
                        .width(Length::Fixed(30.0)),
                    tooltip(
                        button(text(filename_display.clone()).size(normal))
                            .on_press(CsdbBrowserMessage::SelectExtractedFile(file.index))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(if is_selected {
                                button::primary
                            } else {
                                button::text
                            }),
                        text(&file.filename).size(normal),
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                    text(file.ext.to_uppercase())
                        .size(tiny)
                        .width(Length::Fixed(40.0))
                        .color(ext_color),
                    text(size_str.clone())
                        .size(tiny)
                        .width(Length::Fixed(70.0))
                        .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center);

                // Mount buttons for disk images (when connected)
                if is_disk_image && is_connected {
                    let drive_label = self.selected_drive.device_number();

                    file_row = file_row.push(
                        tooltip(
                            button(text(format!("{}:RO", drive_label)).size(tiny))
                                .on_press(CsdbBrowserMessage::MountExtractedFile(
                                    file.index,
                                    MountMode::ReadOnly,
                                ))
                                .padding([4, 6]),
                            text(format!("Mount as Drive {} (Read Only)", drive_label))
                                .size(normal),
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );

                    file_row = file_row.push(
                        tooltip(
                            button(text(format!("{}:RW", drive_label)).size(tiny))
                                .on_press(CsdbBrowserMessage::MountExtractedFile(
                                    file.index,
                                    MountMode::ReadWrite,
                                ))
                                .padding([4, 6]),
                            text(format!("Mount as Drive {} (Read/Write)", drive_label))
                                .size(normal),
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );
                }

                // Run button for runnable files (when connected)
                if is_runnable && is_connected {
                    file_row = file_row.push(
                        tooltip(
                            button(text("▶").size(small))
                                .on_press(CsdbBrowserMessage::RunExtractedFile(file.index))
                                .padding([4, 8]),
                            if is_disk_image {
                                "Mount, reset, and run (LOAD\"*\",8,1 + RUN)"
                            } else {
                                "Run on Ultimate64"
                            },
                            tooltip::Position::Left,
                        )
                        .style(container::bordered_box),
                    );
                }

                file_items.push(file_row.padding([2, 0]).into());
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
}

// Trait to abstract over LatestRelease and SearchResult
trait ReleaseItem {
    fn title(&self) -> &str;
    fn url(&self) -> &str;
    fn id(&self) -> Option<&str>;
    fn group(&self) -> Option<&str>;
    fn release_type(&self) -> Option<&str>;
}

impl ReleaseItem for LatestRelease {
    fn title(&self) -> &str {
        &self.title
    }

    fn url(&self) -> &str {
        &self.release_url
    }

    fn id(&self) -> Option<&str> {
        Some(&self.release_id)
    }

    fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    fn release_type(&self) -> Option<&str> {
        self.release_type.as_deref()
    }
}

impl ReleaseItem for SearchResult {
    fn title(&self) -> &str {
        &self.title
    }

    fn url(&self) -> &str {
        &self.release_url
    }

    fn id(&self) -> Option<&str> {
        self.release_id.as_deref()
    }

    fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    fn release_type(&self) -> Option<&str> {
        self.release_type.as_deref()
    }
}

impl Default for CsdbBrowser {
    fn default() -> Self {
        Self::new()
    }
}