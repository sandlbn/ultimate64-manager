//! Profile Manager UI.
//!
//! Provides a complete UI for managing Ultimate64 device profiles:
//! - Import .cfg and .json files
//! - Browse and select profiles from the git-backed repository
//! - Edit profile settings (reusing config editor patterns)
//! - Compare profiles against baselines (diff view)
//! - Export as .cfg
//! - Apply profiles live to connected devices
//! - Git operations (commit, view history)

use crate::cfg_format;
use crate::config_api;
use crate::device_profile::{self, ConfigTree, DeviceProfile, MountEntry, ProfileMode};
use crate::profile_api;
use crate::profile_repo::{self, ProfileEntry, ProfileRepo};
use iced::{
    widget::{
        button, column, container, pick_list, row, rule, scrollable, text, text_input, toggler,
        tooltip, Column, Row, Space,
    },
    Element, Length, Task,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// ─── Remote file picker state (for path-like config fields) ──────
/// Why: some config keys (e.g. "REU Preload Image") point to paths on the
/// device filesystem. The user shouldn't have to type them manually — the
/// picker lets them FTP-browse the device and pick a file.
pub struct RemotePickerState {
    /// The config category the picker is writing into (e.g. "C64 and Cartridge Settings")
    pub target_category: String,
    /// The config key being edited (e.g. "REU Preload Image")
    pub target_key: String,
    /// Current FTP path being browsed
    pub current_path: String,
    /// Manual path input (for jumping to hidden mount points like /USB0/)
    pub path_input: String,
    /// File listing for the current path
    pub entries: Vec<crate::ftp_ops::RemoteFileEntry>,
    /// Loading state
    pub is_loading: bool,
    /// Error message if any
    pub error: Option<String>,
}

// ─── Popular categories (quick-pick shortcuts for per-game profiles) ──────
// These are common Ultimate64 configuration categories that users typically
// want to tweak for specific games/demos. Ordered by typical use frequency.
const POPULAR_CATEGORIES: &[&str] = &[
    "U64 Specific Settings",      // PAL/NTSC, CPU speed, badline timing
    "SID Sockets Configuration",  // Enable/disable SID chips
    "UltiSID Configuration",      // SID filter curves, resonance
    "SID Addressing",             // SID chip addresses
    "Audio Mixer",                // Volume levels
    "Drive A Settings",           // Drive A enable/type
    "Drive B Settings",           // Drive B enable/type
    "C64 and Cartridge Settings", // REU, cartridge prefs
    "LED Strip Settings",         // Visual effects
];

// ─── Messages ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ProfileManagerMessage {
    // Repository
    InitRepo,
    RepoInitialized(Result<String, String>),
    RefreshProfiles,
    ProfilesLoaded(Result<Vec<ProfileEntry>, String>),

    // New / Clone
    NewProfile,
    CloneProfile(usize),

    // Import
    ImportCfg,
    ImportCfgFileSelected(Option<PathBuf>),
    ImportCfgLoaded(Result<(DeviceProfile, String), String>),
    ImportJson,
    ImportJsonFileSelected(Option<PathBuf>),
    ImportJsonLoaded(Result<DeviceProfile, String>),
    SnapshotFromDevice,
    SnapshotComplete(Result<DeviceProfile, String>),

    // Profile selection and loading
    SelectProfile(usize),
    ProfileLoaded(Result<DeviceProfile, String>),

    // Profile editing — metadata
    EditName(String),
    EditDescription(String),
    EditCategory(String),
    EditTagsInput(String),
    SetProfileMode(String),

    // Profile editing — config (reuses config editor widget patterns)
    SelectConfigCategory(String),
    /// Deselect the current config category to return to the category picker
    DeselectConfigCategory,
    ConfigValueChanged(String, String, serde_json::Value),
    AddCategory(String),
    /// Add a category to the profile (empty — user picks keys to override)
    AddCategoryFromSchema(String),
    /// Add a single key within a category, using its baseline/default value
    AddKeyFromSchema(String, String),
    /// Add every key from the baseline for a category (Add All shortcut)
    AddAllKeysFromSchema(String),

    // ── Remote FTP picker (for path-like config fields) ──
    /// Open the picker for a specific category/key, starting at a default path
    OpenRemotePicker(String, String, String),
    /// Navigate to a different path within the picker
    RemotePickerNavigate(String),
    /// Manual path entry changed
    RemotePickerPathInputChanged(String),
    /// Submit manual path (Enter pressed or Go button)
    RemotePickerGoToPath,
    /// Received a file listing
    RemotePickerListed(Result<(String, Vec<crate::ftp_ops::RemoteFileEntry>), String>),
    /// User picked a file — write its path into the config field and close
    RemotePickerSelect(String),
    /// Close the picker without selecting
    RemotePickerClose,
    RemoveCategory(String),
    RemoveKey(String, String),
    NewCategoryInput(String),
    SearchChanged(String),

    // Mount editing (local paths get uploaded, device paths mounted directly)
    DriveAPathChanged(String),
    DriveAAutoloadChanged(bool),
    BrowseDriveA,
    BrowseDriveASelected(Option<PathBuf>),
    DriveBPathChanged(String),
    DriveBAutoloadChanged(bool),
    BrowseDriveB,
    BrowseDriveBSelected(Option<PathBuf>),
    CartridgePathChanged(String),
    BrowseCartridge,
    BrowseCartridgeSelected(Option<PathBuf>),
    ClearDriveA,
    ClearDriveB,
    ClearCartridge,

    // Launch settings
    RestoreBaselineChanged(bool),
    ResetAfterApplyChanged(bool),

    // Save / Delete
    SaveProfile,
    SaveProfileComplete(Result<String, String>),
    DeleteProfile,
    DeleteProfileFromList(usize),
    DeleteProfileComplete(Result<String, String>),

    // Export
    ExportCfg,
    ExportCfgFileSelected(Option<PathBuf>),
    ExportCfgComplete(Result<String, String>),
    ExportJson,
    ExportJsonFileSelected(Option<PathBuf>),
    ExportJsonComplete(Result<String, String>),

    // Apply to device
    ApplyProfile,
    /// Apply with pre-clean mode: 0=direct, 1=reboot first, 2=load flash defaults first
    ApplyProfileConfirmed(u8),
    ApplyProfileFromList(usize),
    ApplyProfileFromListConfirmed(usize, u8),
    ApplyProfileComplete(Result<String, String>),
    /// Show/hide the apply confirmation dialog
    ShowApplyDialog,
    ShowApplyDialogForList(usize),
    DismissApplyDialog,

    // Screenshot capture from streaming frame buffer
    CaptureScreenshot,
    /// Open a file dialog to pick an image from disk (PNG/JPG/etc.)
    PickImage,
    /// The user picked an image file — decode and store as pending screenshot
    PickImageSelected(Option<PathBuf>),

    // Baseline — captured once from device, stored in repo, loaded at startup
    SnapshotBaseline,
    BaselineSnapshotted(Result<(ConfigTree, crate::device_profile::ConfigSchema), String>),
    ToggleDiffView,

    // Git operations
    CommitChanges,
    CommitComplete(Result<String, String>),
    ViewHistory,
    HistoryLoaded(Result<Vec<String>, String>),

    // View switching
    SwitchView(ViewMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    ProfileList,
    ProfileEditor,
    DiffView,
    RawJson,
    RenderedCfg,
}

// ─── State ────────────────────────────────────────────────────────

pub struct ProfileManager {
    // Repository
    repo_root: PathBuf,
    repo_initialized: bool,
    profiles: Vec<ProfileEntry>,

    // Current profile being edited
    current_profile: Option<DeviceProfile>,
    original_cfg_content: Option<String>,
    save_category: String,
    is_dirty: bool,

    // Config editing state
    selected_config_category: Option<String>,
    search_filter: String,
    new_category_input: String,
    tags_input: String,

    // Streaming frame buffer reference for screenshot capture
    streaming_frame: Option<Arc<std::sync::Mutex<Option<crate::streaming::ScaledFrame>>>>,
    // Captured screenshot PNG bytes (held in memory until profile is saved)
    pending_screenshot: Option<Vec<u8>>,

    // Where the current profile was loaded from, so we can delete the old
    // directory on rename (change of name -> change of id -> new dir).
    original_profile_dir: Option<PathBuf>,

    // Remote file picker popup state
    remote_picker: Option<RemotePickerState>,

    // Baseline for diff view
    baseline_config: Option<ConfigTree>,
    config_schema: Option<crate::device_profile::ConfigSchema>,
    show_diff: bool,

    // UI state
    view_mode: ViewMode,
    pub is_loading: bool,
    /// Pending apply confirmation: None = no dialog, Some(None) = from editor, Some(Some(idx)) = from list
    apply_confirm: Option<Option<usize>>,
    status_message: Option<String>,
    error_message: Option<String>,
    git_history: Vec<String>,
}

impl ProfileManager {
    pub fn new() -> Self {
        let repo_root = ProfileRepo::default_path().unwrap_or_else(|| PathBuf::from("."));
        let repo_initialized = repo_root.join(".git").is_dir();

        // Try to load stored baseline + schema from repo at startup
        let (baseline_config, config_schema) = if repo_initialized {
            let repo = ProfileRepo::new(repo_root.clone());
            match repo.load_baseline("default") {
                Ok(stored) => {
                    let count: usize = stored.config.values().map(|v| v.len()).sum();
                    let schema_count: usize = stored
                        .schema
                        .as_ref()
                        .map(|s| s.values().map(|v| v.len()).sum())
                        .unwrap_or(0);
                    log::info!(
                        "Loaded stored baseline: {} categories, {} settings, {} schema entries",
                        stored.config.len(),
                        count,
                        schema_count,
                    );
                    (Some(stored.config), stored.schema)
                }
                Err(_) => {
                    log::info!("No stored baseline found — snapshot device to create one");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        let has_baseline = baseline_config.is_some();

        // Load profiles from repo at startup
        let profiles = if repo_initialized {
            let repo = ProfileRepo::new(repo_root.clone());
            match repo.list_profiles() {
                Ok(p) => {
                    log::info!("Loaded {} profiles from repository", p.len());
                    p
                }
                Err(e) => {
                    log::warn!("Failed to load profiles: {}", e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        Self {
            repo_root,
            repo_initialized,
            profiles,
            current_profile: None,
            original_cfg_content: None,
            save_category: "uncategorized".to_string(),
            is_dirty: false,
            selected_config_category: None,
            search_filter: String::new(),
            new_category_input: String::new(),
            tags_input: String::new(),
            streaming_frame: None,
            pending_screenshot: None,
            original_profile_dir: None,
            remote_picker: None,
            baseline_config,
            config_schema,
            show_diff: false,
            view_mode: ViewMode::ProfileList,
            is_loading: false,
            apply_confirm: None,
            status_message: Some(if has_baseline {
                "Baseline loaded from repository".to_string()
            } else {
                "Initialize repository and snapshot device baseline to begin".to_string()
            }),
            error_message: None,
            git_history: Vec::new(),
        }
    }

    /// Set the streaming frame buffer reference (called from main on each update).
    pub fn set_streaming_frame(
        &mut self,
        frame: Arc<std::sync::Mutex<Option<crate::streaming::ScaledFrame>>>,
    ) {
        self.streaming_frame = Some(frame);
    }

    pub fn update(
        &mut self,
        message: ProfileManagerMessage,
        host_url: Option<String>,
        password: Option<String>,
        connection: Option<Arc<tokio::sync::Mutex<ultimate64::Rest>>>,
    ) -> Task<ProfileManagerMessage> {
        match message {
            // ── Repository ──
            ProfileManagerMessage::InitRepo => {
                self.is_loading = true;
                self.status_message = Some("Initializing repository...".to_string());
                let root = self.repo_root.clone();
                Task::perform(
                    profile_repo::init_repo_async(root),
                    ProfileManagerMessage::RepoInitialized,
                )
            }
            ProfileManagerMessage::RepoInitialized(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.repo_initialized = true;
                        self.status_message = Some(msg);
                        self.error_message = None;
                        // Refresh profiles list, then auto-snapshot baseline if connected
                        let refresh = self.update(
                            ProfileManagerMessage::RefreshProfiles,
                            host_url.clone(),
                            password.clone(),
                            connection.clone(),
                        );
                        if host_url.is_some() && self.baseline_config.is_none() {
                            let snapshot = self.update(
                                ProfileManagerMessage::SnapshotBaseline,
                                host_url,
                                password,
                                connection,
                            );
                            return Task::batch([refresh, snapshot]);
                        }
                        return refresh;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Init failed: {}", e));
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::RefreshProfiles => {
                self.is_loading = true;
                let root = self.repo_root.clone();
                Task::perform(
                    profile_repo::list_profiles_async(root),
                    ProfileManagerMessage::ProfilesLoaded,
                )
            }
            ProfileManagerMessage::ProfilesLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(profiles) => {
                        self.status_message =
                            Some(format!("{} profiles in repository", profiles.len()));
                        self.profiles = profiles;
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Failed to load profiles: {}", e));
                    }
                }
                Task::none()
            }

            // ── New / Clone ──
            ProfileManagerMessage::NewProfile => {
                let mut profile = DeviceProfile::new("new-profile", "New Profile");
                profile.profile_mode = ProfileMode::Overlay;
                self.current_profile = Some(profile);
                self.tags_input.clear();
                self.save_category = "uncategorized".to_string();
                self.original_profile_dir = None; // brand new — nothing to replace
                self.pending_screenshot = None;
                self.is_dirty = true;
                self.view_mode = ViewMode::ProfileEditor;
                self.status_message = Some("New empty profile created".to_string());
                Task::none()
            }
            ProfileManagerMessage::CloneProfile(index) => {
                if let Some(entry) = self.profiles.get(index) {
                    let path = entry.path.clone();
                    let orig_name = entry.name.clone();
                    // Inherit the category from the source profile
                    self.save_category = entry.category.clone();
                    // Clone is a brand new profile — don't treat it as renaming the original
                    self.original_profile_dir = None;
                    self.is_loading = true;
                    self.status_message = Some(format!("Cloning '{}'...", orig_name));
                    Task::perform(profile_repo::load_profile_async(path), move |result| {
                        match result {
                            Ok(mut profile) => {
                                profile.name = format!("{} (copy)", orig_name);
                                profile.id = device_profile::slugify(&profile.name);
                                profile.metadata.created_at = chrono::Utc::now().to_rfc3339();
                                // Clear screenshot — clone should not inherit it
                                profile.metadata.screenshot.clear();
                                ProfileManagerMessage::ProfileLoaded(Ok(profile))
                            }
                            Err(e) => ProfileManagerMessage::ProfileLoaded(Err(e)),
                        }
                    })
                } else {
                    Task::none()
                }
            }

            // ── Import CFG ──
            ProfileManagerMessage::ImportCfg => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Import Ultimate64 .cfg File")
                        .add_filter("CFG files", &["cfg"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                ProfileManagerMessage::ImportCfgFileSelected,
            ),
            ProfileManagerMessage::ImportCfgFileSelected(path) => {
                if let Some(path) = path {
                    self.is_loading = true;
                    self.status_message = Some("Importing .cfg file...".to_string());
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("imported")
                        .to_string();
                    Task::perform(
                        async move {
                            let content = tokio::fs::read_to_string(&path)
                                .await
                                .map_err(|e| format!("Failed to read file: {}", e))?;
                            let profile = cfg_format::import_cfg(&content, &name)?;
                            Ok((profile, content))
                        },
                        ProfileManagerMessage::ImportCfgLoaded,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ImportCfgLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok((profile, cfg_content)) => {
                        self.status_message = Some(format!(
                            "Imported '{}': {} categories, {} settings",
                            profile.name,
                            profile.config.len(),
                            profile.setting_count()
                        ));
                        self.tags_input = profile.tags.join(", ");
                        self.original_cfg_content = Some(cfg_content);
                        self.current_profile = Some(profile);
                        self.is_dirty = true;
                        self.view_mode = ViewMode::ProfileEditor;
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Import failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Import JSON ──
            ProfileManagerMessage::ImportJson => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Import JSON Profile/Backup")
                        .add_filter("JSON files", &["json"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                ProfileManagerMessage::ImportJsonFileSelected,
            ),
            ProfileManagerMessage::ImportJsonFileSelected(path) => {
                if let Some(path) = path {
                    self.is_loading = true;
                    self.status_message = Some("Importing JSON...".to_string());
                    Task::perform(
                        async move {
                            let content = tokio::fs::read_to_string(&path)
                                .await
                                .map_err(|e| format!("Failed to read file: {}", e))?;
                            device_profile::import_json_backup(&content)
                        },
                        ProfileManagerMessage::ImportJsonLoaded,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ImportJsonLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(profile) => {
                        self.status_message = Some(format!(
                            "Imported '{}': {} categories, {} settings",
                            profile.name,
                            profile.config.len(),
                            profile.setting_count()
                        ));
                        self.tags_input = profile.tags.join(", ");
                        self.current_profile = Some(profile);
                        self.is_dirty = true;
                        self.view_mode = ViewMode::ProfileEditor;
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Import failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Snapshot from device ──
            ProfileManagerMessage::SnapshotFromDevice => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message = Some("Reading all config from device...".to_string());
                    Task::perform(
                        profile_api::snapshot_current_config(
                            host,
                            "Device Snapshot".to_string(),
                            password,
                        ),
                        ProfileManagerMessage::SnapshotComplete,
                    )
                } else {
                    self.error_message = Some("Not connected to device".to_string());
                    Task::none()
                }
            }
            ProfileManagerMessage::SnapshotComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(profile) => {
                        self.status_message = Some(format!(
                            "Snapshot: {} categories, {} settings",
                            profile.config.len(),
                            profile.setting_count()
                        ));
                        self.tags_input = profile.tags.join(", ");
                        self.current_profile = Some(profile);
                        self.is_dirty = true;
                        self.view_mode = ViewMode::ProfileEditor;
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Snapshot failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Profile selection ──
            ProfileManagerMessage::SelectProfile(index) => {
                if let Some(entry) = self.profiles.get(index) {
                    let path = entry.path.clone();
                    // Remember the category so Save puts it back in the right place
                    self.save_category = entry.category.clone();
                    // Remember the original directory so Save can clean up on rename
                    self.original_profile_dir = path.parent().map(|p| p.to_path_buf());
                    self.is_loading = true;
                    self.status_message = Some(format!("Loading '{}'...", entry.name));
                    Task::perform(
                        profile_repo::load_profile_async(path),
                        ProfileManagerMessage::ProfileLoaded,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ProfileLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(profile) => {
                        let is_clone = profile.name.ends_with(" (copy)");
                        self.status_message = Some(format!(
                            "Loaded '{}': {} categories, {} settings",
                            profile.name,
                            profile.config.len(),
                            profile.setting_count()
                        ));
                        self.tags_input = profile.tags.join(", ");
                        self.current_profile = Some(profile);
                        self.is_dirty = is_clone; // Clone needs saving, loaded profile doesn't
                        self.pending_screenshot = None;
                        self.view_mode = ViewMode::ProfileEditor;
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Load failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Metadata editing ──
            ProfileManagerMessage::EditName(name) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.name = name.clone();
                    profile.id = device_profile::slugify(&name);
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::EditDescription(desc) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.description = desc;
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::EditCategory(cat) => {
                self.save_category = cat;
                self.is_dirty = true;
                Task::none()
            }
            ProfileManagerMessage::EditTagsInput(input) => {
                self.tags_input = input.clone();
                if let Some(profile) = &mut self.current_profile {
                    profile.tags = input
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::SetProfileMode(mode) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.profile_mode = match mode.as_str() {
                        "Overlay" => ProfileMode::Overlay,
                        _ => ProfileMode::Full,
                    };
                    self.is_dirty = true;
                }
                Task::none()
            }

            // ── Config editing ──
            ProfileManagerMessage::SelectConfigCategory(cat) => {
                self.selected_config_category = Some(cat);
                self.search_filter.clear();
                Task::none()
            }
            ProfileManagerMessage::DeselectConfigCategory => {
                self.selected_config_category = None;
                self.search_filter.clear();
                Task::none()
            }
            ProfileManagerMessage::ConfigValueChanged(category, key, value) => {
                if let Some(profile) = &mut self.current_profile {
                    profile
                        .config
                        .entry(category)
                        .or_default()
                        .insert(key, value);
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::NewCategoryInput(input) => {
                self.new_category_input = input;
                Task::none()
            }
            ProfileManagerMessage::AddCategory(name) => {
                if !name.is_empty() {
                    if let Some(profile) = &mut self.current_profile {
                        profile.config.entry(name.clone()).or_default();
                        self.is_dirty = true;
                        self.selected_config_category = Some(name);
                    }
                    self.new_category_input.clear();
                }
                Task::none()
            }
            ProfileManagerMessage::AddCategoryFromSchema(name) => {
                // Add category as empty — user will pick individual keys to override
                if let Some(profile) = &mut self.current_profile {
                    profile.config.entry(name.clone()).or_default();
                    self.is_dirty = true;
                    self.selected_config_category = Some(name);
                }
                Task::none()
            }
            ProfileManagerMessage::AddAllKeysFromSchema(category) => {
                // "Add all" shortcut: copy every baseline key in this category
                if let Some(profile) = &mut self.current_profile {
                    let entry = profile.config.entry(category.clone()).or_default();
                    if let Some(baseline) = &self.baseline_config {
                        if let Some(base_items) = baseline.get(&category) {
                            for (k, v) in base_items {
                                entry.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }

            // ── Remote FTP picker ──
            ProfileManagerMessage::OpenRemotePicker(category, key, start_path) => {
                let host = match host_url.clone() {
                    Some(h) => h,
                    None => {
                        self.error_message = Some("Not connected to device".to_string());
                        return Task::none();
                    }
                };
                // Resolve starting directory: if start_path is a file path, browse its parent
                let initial = if start_path.is_empty() {
                    "/".to_string()
                } else if start_path.ends_with('/') {
                    start_path
                } else {
                    // Treat as file path — browse its parent directory
                    match start_path.rsplit_once('/') {
                        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                        _ => "/".to_string(),
                    }
                };
                self.remote_picker = Some(RemotePickerState {
                    target_category: category,
                    target_key: key,
                    current_path: initial.clone(),
                    path_input: initial.clone(),
                    entries: Vec::new(),
                    is_loading: true,
                    error: None,
                });
                let bare = crate::profile_api::bare_host_pub(&host).to_string();
                let path = initial.clone();
                Task::perform(
                    async move {
                        let entries =
                            crate::ftp_ops::fetch_files_ftp(bare, path.clone(), password).await?;
                        Ok::<_, String>((path, entries))
                    },
                    ProfileManagerMessage::RemotePickerListed,
                )
            }
            ProfileManagerMessage::RemotePickerNavigate(path) => {
                let host = match host_url.clone() {
                    Some(h) => h,
                    None => return Task::none(),
                };
                if let Some(picker) = &mut self.remote_picker {
                    picker.is_loading = true;
                    picker.current_path = path.clone();
                    picker.path_input = path.clone();
                    picker.error = None;
                }
                let bare = crate::profile_api::bare_host_pub(&host).to_string();
                Task::perform(
                    async move {
                        let entries =
                            crate::ftp_ops::fetch_files_ftp(bare, path.clone(), password).await?;
                        Ok::<_, String>((path, entries))
                    },
                    ProfileManagerMessage::RemotePickerListed,
                )
            }
            ProfileManagerMessage::RemotePickerPathInputChanged(input) => {
                if let Some(picker) = &mut self.remote_picker {
                    picker.path_input = input;
                }
                Task::none()
            }
            ProfileManagerMessage::RemotePickerGoToPath => {
                let path = match &self.remote_picker {
                    Some(p) => p.path_input.clone(),
                    None => return Task::none(),
                };
                // Route through RemotePickerNavigate so the state is updated consistently
                return self.update(
                    ProfileManagerMessage::RemotePickerNavigate(path),
                    host_url,
                    password,
                    connection,
                );
            }
            ProfileManagerMessage::RemotePickerListed(result) => {
                if let Some(picker) = &mut self.remote_picker {
                    picker.is_loading = false;
                    match result {
                        Ok((path, entries)) => {
                            picker.current_path = path;
                            picker.entries = entries;
                            picker.error = None;
                        }
                        Err(e) => {
                            picker.error = Some(e);
                            picker.entries.clear();
                        }
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::RemotePickerSelect(path) => {
                if let Some(picker) = self.remote_picker.take() {
                    if let Some(profile) = &mut self.current_profile {
                        profile
                            .config
                            .entry(picker.target_category)
                            .or_default()
                            .insert(picker.target_key, serde_json::Value::String(path));
                        self.is_dirty = true;
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::RemotePickerClose => {
                self.remote_picker = None;
                Task::none()
            }
            ProfileManagerMessage::AddKeyFromSchema(category, key) => {
                if let Some(profile) = &mut self.current_profile {
                    // Use the baseline value (captured from the device)
                    let value = self
                        .baseline_config
                        .as_ref()
                        .and_then(|b| b.get(&category))
                        .and_then(|c| c.get(&key))
                        .cloned()
                        .unwrap_or(serde_json::Value::String(String::new()));
                    profile
                        .config
                        .entry(category)
                        .or_default()
                        .insert(key, value);
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::RemoveCategory(name) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.config.remove(&name);
                    if self.selected_config_category.as_ref() == Some(&name) {
                        self.selected_config_category = None;
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::RemoveKey(category, key) => {
                if let Some(profile) = &mut self.current_profile {
                    if let Some(items) = profile.config.get_mut(&category) {
                        items.remove(&key);
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::SearchChanged(filter) => {
                self.search_filter = filter;
                Task::none()
            }

            // ── Mount editing ──
            ProfileManagerMessage::DriveAPathChanged(path) => {
                if let Some(profile) = &mut self.current_profile {
                    if path.is_empty() {
                        profile.mounts.drive_a = None;
                    } else {
                        let entry = profile.mounts.drive_a.get_or_insert(MountEntry {
                            media_type: "disk".to_string(),
                            path: String::new(),
                            autoload: false,
                        });
                        entry.path = path;
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::DriveAAutoloadChanged(val) => {
                if let Some(profile) = &mut self.current_profile {
                    if let Some(entry) = &mut profile.mounts.drive_a {
                        entry.autoload = val;
                        self.is_dirty = true;
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::BrowseDriveA => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Select Disk Image for Drive A")
                        .add_filter("Disk images", &["d64", "g64", "d71", "d81", "t64", "prg"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                ProfileManagerMessage::BrowseDriveASelected,
            ),
            ProfileManagerMessage::BrowseDriveASelected(path) => {
                if let Some(path) = path {
                    let path_str = path.to_string_lossy().to_string();
                    return self.update(
                        ProfileManagerMessage::DriveAPathChanged(path_str),
                        host_url,
                        password,
                        connection,
                    );
                }
                Task::none()
            }
            ProfileManagerMessage::DriveBPathChanged(path) => {
                if let Some(profile) = &mut self.current_profile {
                    if path.is_empty() {
                        profile.mounts.drive_b = None;
                    } else {
                        let entry = profile.mounts.drive_b.get_or_insert(MountEntry {
                            media_type: "disk".to_string(),
                            path: String::new(),
                            autoload: false,
                        });
                        entry.path = path;
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::DriveBAutoloadChanged(val) => {
                if let Some(profile) = &mut self.current_profile {
                    if let Some(entry) = &mut profile.mounts.drive_b {
                        entry.autoload = val;
                        self.is_dirty = true;
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::BrowseDriveB => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Select Disk Image for Drive B")
                        .add_filter("Disk images", &["d64", "g64", "d71", "d81", "t64", "prg"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                ProfileManagerMessage::BrowseDriveBSelected,
            ),
            ProfileManagerMessage::BrowseDriveBSelected(path) => {
                if let Some(path) = path {
                    let path_str = path.to_string_lossy().to_string();
                    return self.update(
                        ProfileManagerMessage::DriveBPathChanged(path_str),
                        host_url,
                        password,
                        connection,
                    );
                }
                Task::none()
            }
            ProfileManagerMessage::CartridgePathChanged(path) => {
                if let Some(profile) = &mut self.current_profile {
                    if path.is_empty() {
                        profile.mounts.cartridge = None;
                    } else {
                        let entry = profile.mounts.cartridge.get_or_insert(MountEntry {
                            media_type: "cartridge".to_string(),
                            path: String::new(),
                            autoload: false,
                        });
                        entry.path = path;
                    }
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::BrowseCartridge => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .set_title("Select Cartridge / Program")
                        .add_filter("Cart / program / music", &["crt", "prg", "sid"])
                        .add_filter("Cartridge", &["crt"])
                        .add_filter("Program", &["prg"])
                        .add_filter("SID music", &["sid"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                ProfileManagerMessage::BrowseCartridgeSelected,
            ),
            ProfileManagerMessage::BrowseCartridgeSelected(path) => {
                if let Some(path) = path {
                    let path_str = path.to_string_lossy().to_string();
                    return self.update(
                        ProfileManagerMessage::CartridgePathChanged(path_str),
                        host_url,
                        password,
                        connection,
                    );
                }
                Task::none()
            }
            ProfileManagerMessage::ClearDriveA => {
                if let Some(profile) = &mut self.current_profile {
                    profile.mounts.drive_a = None;
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::ClearDriveB => {
                if let Some(profile) = &mut self.current_profile {
                    profile.mounts.drive_b = None;
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::ClearCartridge => {
                if let Some(profile) = &mut self.current_profile {
                    profile.mounts.cartridge = None;
                    self.is_dirty = true;
                }
                Task::none()
            }

            // ── Launch settings ──
            ProfileManagerMessage::RestoreBaselineChanged(val) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.launch.restore_baseline_first = val;
                    self.is_dirty = true;
                }
                Task::none()
            }
            ProfileManagerMessage::ResetAfterApplyChanged(val) => {
                if let Some(profile) = &mut self.current_profile {
                    profile.launch.reset_after_apply = val;
                    self.is_dirty = true;
                }
                Task::none()
            }

            // ── Save ──
            ProfileManagerMessage::SaveProfile => {
                if let Some(profile) = &self.current_profile {
                    self.is_loading = true;
                    self.status_message = Some("Saving profile...".to_string());
                    let root = self.repo_root.clone();
                    let profile = profile.clone();
                    let category = self.save_category.clone();
                    let original_cfg = self.original_cfg_content.clone();
                    let screenshot_data = self.pending_screenshot.take();
                    // If the profile was loaded from an existing directory and
                    // its id/category changed (rename), we need to delete the
                    // old directory so we don't leave a stale duplicate.
                    let old_dir = {
                        let new_cat = if category.is_empty() {
                            "uncategorized".to_string()
                        } else {
                            category.clone()
                        };
                        let new_dir = root.join("profiles").join(&new_cat).join(&profile.id);
                        self.original_profile_dir
                            .as_ref()
                            .filter(|p| **p != new_dir)
                            .cloned()
                    };
                    Task::perform(
                        async move {
                            // Save profile
                            let result = profile_repo::save_profile_async(
                                root.clone(),
                                profile.clone(),
                                category.clone(),
                                original_cfg,
                                None,
                            )
                            .await?;

                            // Write pending screenshot if any
                            let cat = if category.is_empty() {
                                "uncategorized".to_string()
                            } else {
                                category
                            };
                            let new_profile_dir =
                                root.join("profiles").join(&cat).join(&profile.id);

                            if let Some(png_bytes) = screenshot_data {
                                let dest = new_profile_dir.join("screenshot.png");
                                tokio::fs::write(&dest, &png_bytes)
                                    .await
                                    .map_err(|e| format!("Failed to save screenshot: {}", e))?;
                                log::info!("Saved screenshot to {}", dest.display());
                            }

                            // Rename: migrate the old directory's extra files
                            // (screenshot, original.cfg) to the new location
                            // and delete the old directory + commit.
                            if let Some(old) = old_dir {
                                if old.exists() && old != new_profile_dir {
                                    // Copy over screenshot.png if present and not already in new
                                    let old_ss = old.join("screenshot.png");
                                    let new_ss = new_profile_dir.join("screenshot.png");
                                    if old_ss.exists() && !new_ss.exists() {
                                        let _ = tokio::fs::copy(&old_ss, &new_ss).await;
                                    }
                                    // Copy over original.cfg if present and not already in new
                                    let old_cfg = old.join("original.cfg");
                                    let new_cfg = new_profile_dir.join("original.cfg");
                                    if old_cfg.exists() && !new_cfg.exists() {
                                        let _ = tokio::fs::copy(&old_cfg, &new_cfg).await;
                                    }
                                    // Delete the old directory and commit the rename
                                    let root_for_delete = root.clone();
                                    let old_name = old
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    tokio::task::spawn_blocking(move || {
                                        let mut repo =
                                            crate::profile_repo::ProfileRepo::new(root_for_delete);
                                        if let Err(e) = std::fs::remove_dir_all(&old) {
                                            log::warn!(
                                                "Failed to remove old profile dir {}: {}",
                                                old.display(),
                                                e
                                            );
                                        }
                                        let _ = repo.commit(&format!(
                                            "Rename profile: {} -> {}",
                                            old_name, profile.id
                                        ));
                                    })
                                    .await
                                    .map_err(|e| format!("Task error: {}", e))?;
                                }
                            }

                            Ok(result)
                        },
                        ProfileManagerMessage::SaveProfileComplete,
                    )
                } else {
                    self.error_message = Some("No profile to save".to_string());
                    Task::none()
                }
            }
            ProfileManagerMessage::SaveProfileComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.is_dirty = false;
                        self.error_message = None;
                        // After a successful save the profile now lives at
                        // <root>/profiles/<category>/<id>/, so subsequent saves
                        // of the same profile should NOT trigger rename cleanup.
                        if let Some(profile) = &self.current_profile {
                            let cat = if self.save_category.is_empty() {
                                "uncategorized".to_string()
                            } else {
                                self.save_category.clone()
                            };
                            self.original_profile_dir =
                                Some(self.repo_root.join("profiles").join(&cat).join(&profile.id));
                        }
                        return self.update(
                            ProfileManagerMessage::RefreshProfiles,
                            host_url,
                            password,
                            connection,
                        );
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Save failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Delete ──
            ProfileManagerMessage::DeleteProfile => {
                if let Some(profile) = &self.current_profile {
                    // Find the path from profiles list
                    if let Some(entry) = self.profiles.iter().find(|e| e.id == profile.id) {
                        let root = self.repo_root.clone();
                        let path = entry.path.clone();
                        let name = profile.name.clone();
                        self.is_loading = true;
                        self.status_message = Some(format!("Deleting '{}'...", name));
                        Task::perform(
                            profile_repo::delete_profile_async(root, path, name),
                            ProfileManagerMessage::DeleteProfileComplete,
                        )
                    } else {
                        self.error_message =
                            Some("Profile not yet saved to repository".to_string());
                        Task::none()
                    }
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::DeleteProfileFromList(index) => {
                if let Some(entry) = self.profiles.get(index) {
                    let root = self.repo_root.clone();
                    let path = entry.path.clone();
                    let name = entry.name.clone();
                    self.is_loading = true;
                    self.status_message = Some(format!("Deleting '{}'...", name));
                    Task::perform(
                        profile_repo::delete_profile_async(root, path, name),
                        ProfileManagerMessage::DeleteProfileComplete,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::DeleteProfileComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.current_profile = None;
                        self.view_mode = ViewMode::ProfileList;
                        self.error_message = None;
                        return self.update(
                            ProfileManagerMessage::RefreshProfiles,
                            host_url,
                            password,
                            connection,
                        );
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Delete failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Export CFG ──
            ProfileManagerMessage::ExportCfg => {
                if self.current_profile.is_some() {
                    let default_name = self
                        .current_profile
                        .as_ref()
                        .map(|p| format!("{}.cfg", p.id))
                        .unwrap_or_else(|| "export.cfg".to_string());
                    Task::perform(
                        async move {
                            rfd::AsyncFileDialog::new()
                                .set_title("Export as .cfg")
                                .set_file_name(&default_name)
                                .add_filter("CFG files", &["cfg"])
                                .save_file()
                                .await
                                .map(|h| h.path().to_path_buf())
                        },
                        ProfileManagerMessage::ExportCfgFileSelected,
                    )
                } else {
                    self.error_message = Some("No profile loaded".to_string());
                    Task::none()
                }
            }
            ProfileManagerMessage::ExportCfgFileSelected(path) => {
                if let (Some(path), Some(profile)) = (path, &self.current_profile) {
                    let cfg_content = cfg_format::export_profile_cfg(profile);
                    let profile_name = profile.name.clone();
                    Task::perform(
                        async move {
                            tokio::fs::write(&path, &cfg_content)
                                .await
                                .map_err(|e| format!("Failed to write: {}", e))?;
                            Ok(format!(
                                "Exported '{}' to {}",
                                profile_name,
                                path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                            ))
                        },
                        ProfileManagerMessage::ExportCfgComplete,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ExportCfgComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                    }
                    Err(e) => self.error_message = Some(e),
                }
                Task::none()
            }

            // ── Export JSON ──
            ProfileManagerMessage::ExportJson => {
                if self.current_profile.is_some() {
                    let default_name = self
                        .current_profile
                        .as_ref()
                        .map(|p| format!("{}.json", p.id))
                        .unwrap_or_else(|| "export.json".to_string());
                    Task::perform(
                        async move {
                            rfd::AsyncFileDialog::new()
                                .set_title("Export as JSON")
                                .set_file_name(&default_name)
                                .add_filter("JSON files", &["json"])
                                .save_file()
                                .await
                                .map(|h| h.path().to_path_buf())
                        },
                        ProfileManagerMessage::ExportJsonFileSelected,
                    )
                } else {
                    self.error_message = Some("No profile loaded".to_string());
                    Task::none()
                }
            }
            ProfileManagerMessage::ExportJsonFileSelected(path) => {
                if let (Some(path), Some(profile)) = (path, &self.current_profile) {
                    let json = serde_json::to_string_pretty(profile).unwrap_or_default();
                    let profile_name = profile.name.clone();
                    Task::perform(
                        async move {
                            tokio::fs::write(&path, &json)
                                .await
                                .map_err(|e| format!("Failed to write: {}", e))?;
                            Ok(format!("Exported '{}' as JSON", profile_name,))
                        },
                        ProfileManagerMessage::ExportJsonComplete,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ExportJsonComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                    }
                    Err(e) => self.error_message = Some(e),
                }
                Task::none()
            }

            // ── Apply to device ──
            // Show confirmation dialog first — user chooses Apply or Reboot & Apply
            ProfileManagerMessage::ApplyProfile => {
                if self.is_loading {
                    return Task::none();
                }
                self.apply_confirm = Some(None); // from editor
                Task::none()
            }
            ProfileManagerMessage::ShowApplyDialog => {
                if self.is_loading {
                    return Task::none();
                }
                self.apply_confirm = Some(None);
                Task::none()
            }
            ProfileManagerMessage::ShowApplyDialogForList(index) => {
                if self.is_loading {
                    return Task::none();
                }
                self.apply_confirm = Some(Some(index));
                Task::none()
            }
            ProfileManagerMessage::DismissApplyDialog => {
                self.apply_confirm = None;
                Task::none()
            }
            ProfileManagerMessage::ApplyProfileConfirmed(pre_clean_mode) => {
                self.apply_confirm = None;
                if self.is_loading {
                    log::warn!("Apply already in progress — ignoring duplicate submit");
                    return Task::none();
                }
                let host = match host_url {
                    Some(h) => h,
                    None => {
                        self.error_message = Some("Not connected to device".to_string());
                        return Task::none();
                    }
                };
                let profile = match &self.current_profile {
                    Some(p) => p.clone(),
                    None => {
                        self.error_message = Some("No profile loaded".to_string());
                        return Task::none();
                    }
                };
                let baseline = match &self.baseline_config {
                    Some(b) => b.clone(),
                    None => {
                        self.error_message = Some(
                            "No baseline stored. Click 'Snapshot Baseline' first.".to_string(),
                        );
                        return Task::none();
                    }
                };

                let diff = device_profile::diff_configs(&baseline, &profile.config);
                let diff_count: usize = diff.values().map(|v| v.len()).sum();
                let mode_label = match pre_clean_mode {
                    1 => "Rebooting then ",
                    2 => "Loading flash defaults then ",
                    _ => "",
                };

                self.is_loading = true;
                self.status_message = Some(format!(
                    "{}Applying '{}' — {} settings to change...",
                    mode_label, profile.name, diff_count
                ));

                let conn = connection.clone();
                Task::perform(
                    async move {
                        profile_api::apply_profile(
                            host,
                            &profile,
                            diff,
                            password,
                            conn,
                            pre_clean_mode,
                        )
                        .await
                    },
                    ProfileManagerMessage::ApplyProfileComplete,
                )
            }
            ProfileManagerMessage::ApplyProfileFromList(index) => {
                // Show dialog for list-based apply
                self.apply_confirm = Some(Some(index));
                Task::none()
            }
            ProfileManagerMessage::ApplyProfileFromListConfirmed(index, pre_clean_mode) => {
                self.apply_confirm = None;
                if self.is_loading {
                    log::warn!("Apply already in progress — ignoring duplicate submit");
                    return Task::none();
                }
                if let Some(entry) = self.profiles.get(index) {
                    let host = match host_url {
                        Some(h) => h,
                        None => {
                            self.error_message = Some("Not connected to device".to_string());
                            return Task::none();
                        }
                    };
                    let baseline = match &self.baseline_config {
                        Some(b) => b.clone(),
                        None => {
                            self.error_message =
                                Some("No baseline. Click 'Snapshot Baseline' first.".to_string());
                            return Task::none();
                        }
                    };

                    let path = entry.path.clone();
                    let name = entry.name.clone();
                    let mode_label = match pre_clean_mode {
                        1 => "Rebooting then ",
                        2 => "Loading flash then ",
                        _ => "",
                    };
                    self.is_loading = true;
                    self.status_message = Some(format!("{}Running '{}'...", mode_label, name));

                    let conn = connection.clone();
                    Task::perform(
                        async move {
                            let profile = profile_repo::load_profile_async(path).await?;
                            let diff = device_profile::diff_configs(&baseline, &profile.config);
                            profile_api::apply_profile(
                                host,
                                &profile,
                                diff,
                                password,
                                conn,
                                pre_clean_mode,
                            )
                            .await
                        },
                        ProfileManagerMessage::ApplyProfileComplete,
                    )
                } else {
                    Task::none()
                }
            }
            ProfileManagerMessage::ApplyProfileComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Apply failed: {}", e));
                    }
                }
                Task::none()
            }

            // ── Screenshot from streaming frame buffer ──
            ProfileManagerMessage::CaptureScreenshot => {
                if self.current_profile.is_none() {
                    self.error_message = Some("No profile loaded".to_string());
                    return Task::none();
                }

                // Grab frame from streaming buffer
                let frame_opt = self
                    .streaming_frame
                    .as_ref()
                    .and_then(|fb| fb.lock().ok())
                    .and_then(|guard| guard.clone());

                match frame_opt {
                    Some(frame) => {
                        // Encode RGBA frame to PNG in memory
                        let mut png_bytes: Vec<u8> = Vec::new();
                        {
                            let encoder = png::Encoder::new(
                                std::io::Cursor::new(&mut png_bytes),
                                frame.width,
                                frame.height,
                            );
                            let mut encoder = encoder;
                            encoder.set_color(png::ColorType::Rgba);
                            encoder.set_depth(png::BitDepth::Eight);
                            match encoder.write_header() {
                                Ok(mut writer) => {
                                    if let Err(e) = writer.write_image_data(&frame.data) {
                                        self.error_message =
                                            Some(format!("PNG encode failed: {}", e));
                                        return Task::none();
                                    }
                                }
                                Err(e) => {
                                    self.error_message = Some(format!("PNG header failed: {}", e));
                                    return Task::none();
                                }
                            }
                        }

                        self.pending_screenshot = Some(png_bytes);
                        if let Some(profile) = &mut self.current_profile {
                            profile.metadata.screenshot = "screenshot.png".to_string();
                            self.is_dirty = true;
                        }
                        self.status_message =
                            Some("Screenshot captured from stream (save to persist)".to_string());
                        self.error_message = None;
                    }
                    None => {
                        self.error_message = Some(
                            "No streaming frame available. Start Video Viewer streaming first."
                                .to_string(),
                        );
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::PickImage => {
                if self.current_profile.is_none() {
                    self.error_message = Some("No profile loaded".to_string());
                    return Task::none();
                }
                Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Pick a profile image")
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp"])
                            .add_filter("All files", &["*"])
                            .pick_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    ProfileManagerMessage::PickImageSelected,
                )
            }
            ProfileManagerMessage::PickImageSelected(path) => {
                let Some(path) = path else {
                    return Task::none();
                };
                // Decode via the `image` crate and re-encode as PNG so the
                // profile always has a consistent `screenshot.png` regardless
                // of the source format.
                match image::open(&path) {
                    Ok(img) => {
                        // Downscale to a reasonable thumbnail size if huge
                        // (keeps the repo small; profile lists show 48x36).
                        let max_dim = 1024u32;
                        let img = if img.width() > max_dim || img.height() > max_dim {
                            img.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3)
                        } else {
                            img
                        };
                        let mut png_bytes: Vec<u8> = Vec::new();
                        match img.write_to(
                            &mut std::io::Cursor::new(&mut png_bytes),
                            image::ImageOutputFormat::Png,
                        ) {
                            Ok(()) => {
                                self.pending_screenshot = Some(png_bytes);
                                if let Some(profile) = &mut self.current_profile {
                                    profile.metadata.screenshot = "screenshot.png".to_string();
                                    self.is_dirty = true;
                                }
                                self.status_message = Some(format!(
                                    "Image loaded from {} (save to persist)",
                                    path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
                                ));
                                self.error_message = None;
                            }
                            Err(e) => {
                                self.error_message =
                                    Some(format!("Failed to encode as PNG: {}", e));
                            }
                        }
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Failed to load image: {}", e));
                    }
                }
                Task::none()
            }

            // ── Baseline ──
            // Snapshot device config, store as "default" baseline in repo, keep in memory
            ProfileManagerMessage::SnapshotBaseline => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message =
                        Some("Snapshotting device config + schema (one-time)...".to_string());
                    let root = self.repo_root.clone();
                    Task::perform(
                        async move {
                            // Read all config + schema from device
                            let (config, schema) =
                                profile_api::read_current_config(host, password).await?;

                            // Save to repo (config + schema together)
                            tokio::task::spawn_blocking({
                                let config = config.clone();
                                let schema = schema.clone();
                                move || {
                                    let mut repo = ProfileRepo::new(root);
                                    repo.save_baseline("default", &config, Some(&schema))?;
                                    repo.commit("Snapshot device baseline + schema")?;
                                    Ok::<(), String>(())
                                }
                            })
                            .await
                            .map_err(|e| format!("Task error: {}", e))??;

                            Ok((config, schema))
                        },
                        ProfileManagerMessage::BaselineSnapshotted,
                    )
                } else {
                    self.error_message = Some("Not connected to device".to_string());
                    Task::none()
                }
            }
            ProfileManagerMessage::BaselineSnapshotted(result) => {
                self.is_loading = false;
                match result {
                    Ok((config, schema)) => {
                        let count: usize = config.values().map(|v| v.len()).sum();
                        let schema_count: usize = schema.values().map(|v| v.len()).sum();
                        self.status_message = Some(format!(
                            "Baseline stored: {} categories, {} settings, {} schema entries",
                            config.len(),
                            count,
                            schema_count,
                        ));
                        self.baseline_config = Some(config);
                        self.config_schema = Some(schema);
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Baseline snapshot failed: {}", e));
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::ToggleDiffView => {
                self.show_diff = !self.show_diff;
                Task::none()
            }

            // ── Git ──
            ProfileManagerMessage::CommitChanges => {
                let root = self.repo_root.clone();
                self.is_loading = true;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let mut repo = ProfileRepo::new(root);
                            repo.commit("Manual commit")?;
                            Ok("Changes committed".to_string())
                        })
                        .await
                        .map_err(|e| format!("Task error: {}", e))?
                    },
                    ProfileManagerMessage::CommitComplete,
                )
            }
            ProfileManagerMessage::CommitComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                    }
                    Err(e) => self.error_message = Some(e),
                }
                Task::none()
            }
            ProfileManagerMessage::ViewHistory => {
                let root = self.repo_root.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let repo = ProfileRepo::new(root);
                            repo.git_log(20)
                        })
                        .await
                        .map_err(|e| format!("Task error: {}", e))?
                    },
                    ProfileManagerMessage::HistoryLoaded,
                )
            }
            ProfileManagerMessage::HistoryLoaded(result) => {
                match result {
                    Ok(log) => {
                        self.git_history = log;
                        self.error_message = None;
                    }
                    Err(e) => self.error_message = Some(e),
                }
                Task::none()
            }

            // ── View switching ──
            ProfileManagerMessage::SwitchView(mode) => {
                self.view_mode = mode;
                Task::none()
            }
        }
    }

    // ─── View ─────────────────────────────────────────────────────

    pub fn view(&self, is_connected: bool, font_size: u32) -> Element<'_, ProfileManagerMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        // Remote picker takes over the whole view when open
        if self.remote_picker.is_some() {
            return self.view_remote_picker(fs);
        }

        // Apply confirmation dialog takes over the whole view
        if let Some(ref list_index) = self.apply_confirm {
            return self.view_apply_dialog(*list_index, fs);
        }

        let content: Element<'_, ProfileManagerMessage> = match self.view_mode {
            ViewMode::ProfileList => self.view_profile_list(is_connected, fs),
            ViewMode::ProfileEditor => self.view_profile_editor(is_connected, fs),
            ViewMode::DiffView => self.view_diff(fs),
            ViewMode::RawJson => self.view_raw_json(fs),
            ViewMode::RenderedCfg => self.view_rendered_cfg(fs),
        };

        let status_bar = self.view_status_bar(fs);

        column![content, rule::horizontal(1), status_bar]
            .spacing(0)
            .height(Length::Fill)
            .into()
    }

    /// Render the apply confirmation dialog.
    /// `list_index`: None = apply from editor, Some(idx) = apply from list row
    fn view_apply_dialog(
        &self,
        list_index: Option<usize>,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
        let profile_name = if let Some(idx) = list_index {
            self.profiles
                .get(idx)
                .map(|e| e.name.as_str())
                .unwrap_or("?")
        } else {
            self.current_profile
                .as_ref()
                .map(|p| p.name.as_str())
                .unwrap_or("?")
        };

        let apply_msg = if let Some(idx) = list_index {
            ProfileManagerMessage::ApplyProfileFromListConfirmed(idx, 0)
        } else {
            ProfileManagerMessage::ApplyProfileConfirmed(0)
        };

        let flash_apply_msg = if let Some(idx) = list_index {
            ProfileManagerMessage::ApplyProfileFromListConfirmed(idx, 2)
        } else {
            ProfileManagerMessage::ApplyProfileConfirmed(2)
        };

        container(
            column![
                text(format!("Apply profile: {}", profile_name)).size(fs.large),
                Space::new().height(10),
                text("Choose how to apply:").size(fs.normal),
                Space::new().height(10),
                tooltip(
                    button(text("Apply").size(fs.normal))
                        .on_press(apply_msg)
                        .padding([8, 20])
                        .width(Length::Fixed(320.0))
                        .style(button::success),
                    "Apply the profile settings directly.\nMount+reset handles any previously loaded cartridge.",
                    tooltip::Position::Right,
                )
                .style(container::bordered_box),
                Space::new().height(5),
                tooltip(
                    button(text("Load Flash Defaults & Apply").size(fs.normal))
                        .on_press(flash_apply_msg)
                        .padding([8, 20])
                        .width(Length::Fixed(320.0))
                        .style(button::primary),
                    "Restore saved flash config first (clears runtime changes),\nthen apply the profile.",
                    tooltip::Position::Right,
                )
                .style(container::bordered_box),
                Space::new().height(15),
                button(text("Cancel").size(fs.small))
                    .on_press(ProfileManagerMessage::DismissApplyDialog)
                    .padding([6, 16]),
            ]
            .spacing(5)
            .align_x(iced::Alignment::Center),
        )
        .padding(40)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    /// Render the FTP file picker view (full-screen takeover when active).
    fn view_remote_picker(
        &self,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
        let picker = match &self.remote_picker {
            Some(p) => p,
            None => return Space::new().into(),
        };

        // Header with target info + close button
        let header = container(
            row![
                column![
                    text(format!(
                        "Select file for: [{}] {}",
                        picker.target_category, picker.target_key
                    ))
                    .size(fs.normal),
                    text(&picker.current_path)
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                ]
                .spacing(3),
                Space::new().width(Length::Fill),
                button(text("Cancel").size(fs.small))
                    .on_press(ProfileManagerMessage::RemotePickerClose)
                    .padding([4, 10])
                    .style(button::danger),
            ]
            .spacing(10)
            .height(Length::Shrink)
            .align_y(iced::Alignment::Center),
        )
        .padding(10);

        // Row 1: Up / root / refresh / manual path input
        let parent_path = match picker.current_path.rsplit_once('/') {
            Some((parent, _)) if !parent.is_empty() => parent.to_string(),
            _ => "/".to_string(),
        };
        let nav_row_1 = row![
            tooltip(
                button(text("↑ Up").size(fs.small))
                    .on_press_maybe(if picker.current_path != "/" {
                        Some(ProfileManagerMessage::RemotePickerNavigate(parent_path))
                    } else {
                        None
                    })
                    .padding([4, 10]),
                "Go to parent directory",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            button(text("/").size(fs.small))
                .on_press(ProfileManagerMessage::RemotePickerNavigate("/".to_string()))
                .padding([4, 10]),
            tooltip(
                button(text("Refresh").size(fs.small))
                    .on_press(ProfileManagerMessage::RemotePickerNavigate(
                        picker.current_path.clone(),
                    ))
                    .padding([4, 10]),
                "Re-list current directory (useful after plugging in USB)",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            text_input("Type path, e.g. /USB0/games", &picker.path_input)
                .on_input(ProfileManagerMessage::RemotePickerPathInputChanged)
                .on_submit(ProfileManagerMessage::RemotePickerGoToPath)
                .size(fs.small as f32)
                .width(Length::Fill),
            tooltip(
                button(text("Go").size(fs.small))
                    .on_press(ProfileManagerMessage::RemotePickerGoToPath)
                    .padding([4, 10]),
                "Navigate to typed path — use this for /USB0/, /USB1/, /SD/, /Flash/\n(hidden mount points that FTP doesn't list until mounted)",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .height(Length::Shrink)
        .align_y(iced::Alignment::Center);

        // Row 2: Quick-jump buttons for common mount points.
        // Ultimate firmware hides un-inserted USB from the root listing, so
        // users need a way to jump directly. These are the canonical paths
        // returned by the device's REST API (/v1/drives shows partition paths).
        let quick_jumps = ["/Flash", "/SD", "/Temp", "/USB0", "/USB1", "/USB2", "/USB3"];
        let mut jump_row_items: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();
        jump_row_items.push(
            text("Quick jump:")
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.55, 0.55, 0.6))
                .into(),
        );
        for jp in quick_jumps {
            jump_row_items.push(
                button(text(jp).size(fs.tiny))
                    .on_press(ProfileManagerMessage::RemotePickerNavigate(jp.to_string()))
                    .padding([2, 6])
                    .style(button::text)
                    .into(),
            );
        }
        let nav_row_2 = Row::with_children(jump_row_items)
            .spacing(4)
            .height(Length::Shrink)
            .align_y(iced::Alignment::Center);

        let nav = container(column![nav_row_1, nav_row_2].spacing(4)).padding([0, 10]);

        // File listing
        let listing: Element<'_, ProfileManagerMessage> = if picker.is_loading {
            container(text("Loading...").size(fs.normal))
                .padding(20)
                .center_x(Length::Fill)
                .into()
        } else if let Some(err) = &picker.error {
            container(
                text(format!("Error: {}", err))
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.9, 0.3, 0.3)),
            )
            .padding(20)
            .into()
        } else if picker.entries.is_empty() {
            container(
                text("(empty)")
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
            )
            .padding(20)
            .center_x(Length::Fill)
            .into()
        } else {
            let mut rows: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();
            for entry in &picker.entries {
                let icon = if entry.is_dir { "📁" } else { "📄" };
                let label = format!("{}  {}", icon, entry.name);
                let path = entry.path.clone();
                let msg = if entry.is_dir {
                    ProfileManagerMessage::RemotePickerNavigate(path)
                } else {
                    ProfileManagerMessage::RemotePickerSelect(path)
                };
                let size_text = if entry.is_dir {
                    String::new()
                } else {
                    crate::file_types::format_file_size(entry.size)
                };
                rows.push(
                    button(
                        row![
                            text(label).size(fs.normal).width(Length::Fill),
                            text(size_text)
                                .size(fs.tiny)
                                .color(iced::Color::from_rgb(0.5, 0.5, 0.55)),
                        ]
                        .spacing(10)
                        .align_y(iced::Alignment::Center),
                    )
                    .on_press(msg)
                    .padding([4, 10])
                    .width(Length::Fill)
                    .style(button::text)
                    .into(),
                );
            }
            scrollable(
                Column::with_children(rows)
                    .spacing(2)
                    .padding(iced::Padding::new(5.0).right(15.0)),
            )
            .height(Length::Fill)
            .into()
        };

        column![
            header,
            rule::horizontal(1),
            nav,
            rule::horizontal(1),
            listing
        ]
        .spacing(4)
        .height(Length::Fill)
        .padding(10)
        .into()
    }

    // ── Profile List View ──

    fn view_profile_list(
        &self,
        is_connected: bool,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
        let has_baseline = self.baseline_config.is_some();
        let mut toolbar_items: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();

        if !self.repo_initialized {
            toolbar_items.push(
                button(text("Init Repo").size(fs.small))
                    .on_press(ProfileManagerMessage::InitRepo)
                    .padding([4, 8])
                    .into(),
            );
        }

        toolbar_items.push(
            button(text("New").size(fs.small))
                .on_press_maybe(has_baseline.then_some(ProfileManagerMessage::NewProfile))
                .padding([4, 8])
                .into(),
        );
        // Import .cfg / .json hidden for now — workflow is still rough;
        // baseline snapshot + manual editing is the primary flow.
        toolbar_items.push(
            tooltip(
                button(text("Snapshot Baseline").size(fs.small))
                    .on_press_maybe(
                        is_connected.then_some(ProfileManagerMessage::SnapshotBaseline),
                    )
                    .padding([4, 8])
                    .style(crate::styles::baseline_button),
                if has_baseline {
                    "Re-read all device config + schema. Needed when firmware changes."
                } else {
                    "Read all device config + schema. REQUIRED before creating or applying profiles."
                },
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box)
            .into(),
        );
        toolbar_items.push(Space::new().width(Length::Fill).into());
        toolbar_items.push(
            button(text("Refresh").size(fs.small))
                .on_press(ProfileManagerMessage::RefreshProfiles)
                .padding([4, 8])
                .into(),
        );

        let toolbar = Row::with_children(toolbar_items)
            .spacing(5)
            .height(Length::Shrink)
            .align_y(iced::Alignment::Center);

        let profile_list: Element<'_, ProfileManagerMessage> = if self.profiles.is_empty() {
            container(
                text(if self.repo_initialized {
                    "No profiles yet. Import a .cfg or .json, or snapshot from device."
                } else {
                    "Click 'Init Repo' to create the profile repository."
                })
                .size(fs.normal),
            )
            .padding(20)
            .center_x(Length::Fill)
            .into()
        } else {
            let items: Vec<Element<'_, ProfileManagerMessage>> = self
                .profiles
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let tags = if entry.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", entry.tags.join(", "))
                    };

                    let mut row_items: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();

                    // Thumbnail — always reserve the same width so columns align
                    if let Some(ref ss_path) = entry.screenshot_path {
                        let handle = iced::widget::image::Handle::from_path(ss_path);
                        row_items.push(
                            iced::widget::image(handle)
                                .width(Length::Fixed(48.0))
                                .height(Length::Fixed(36.0))
                                .into(),
                        );
                    } else {
                        row_items.push(
                            Space::new()
                                .width(Length::Fixed(48.0))
                                .height(Length::Fixed(36.0))
                                .into(),
                        );
                    }

                    row_items.push(
                        text(&entry.name)
                            .size(fs.normal)
                            .width(Length::FillPortion(3))
                            .into(),
                    );
                    row_items.push(
                        text(&entry.category)
                            .size(fs.small)
                            .width(Length::FillPortion(2))
                            .color(iced::Color::from_rgb(0.55, 0.55, 0.6))
                            .into(),
                    );
                    row_items.push(
                        text(&entry.mode)
                            .size(fs.small)
                            .width(Length::FillPortion(1))
                            .color(iced::Color::from_rgb(0.55, 0.55, 0.6))
                            .into(),
                    );
                    row_items.push(
                        text(format!("{} settings{}", entry.setting_count, tags))
                            .size(fs.small)
                            .width(Length::FillPortion(2))
                            .color(iced::Color::from_rgb(0.55, 0.55, 0.6))
                            .into(),
                    );

                    row![
                        button(
                            Row::with_children(row_items)
                                .spacing(10)
                                .align_y(iced::Alignment::Center),
                        )
                        .on_press(ProfileManagerMessage::SelectProfile(i))
                        .padding([4, 10])
                        .width(Length::Fill)
                        .style(button::text),
                        button(text("Clone").size(fs.small))
                            .on_press(ProfileManagerMessage::CloneProfile(i))
                            .padding([4, 8]),
                        button(text("Run").size(fs.small))
                            .on_press_maybe(
                                (is_connected && has_baseline)
                                    .then_some(ProfileManagerMessage::ApplyProfileFromList(i),),
                            )
                            .padding([4, 8])
                            .style(button::success),
                        button(text("x").size(fs.small))
                            .on_press(ProfileManagerMessage::DeleteProfileFromList(i))
                            .padding([4, 8])
                            .style(button::danger),
                    ]
                    .spacing(4)
                    .align_y(iced::Alignment::Center)
                    .into()
                })
                .collect();

            scrollable(
                Column::with_children(items)
                    .spacing(2)
                    .padding(iced::Padding::new(5.0).right(15.0)),
            )
            .height(Length::Fill)
            .into()
        };

        // Git history (compact, at bottom if loaded)
        let history_panel: Element<'_, ProfileManagerMessage> = if self.git_history.is_empty() {
            Space::new().height(0).into()
        } else {
            container({
                let history_items: Vec<Element<'_, ProfileManagerMessage>> = self
                    .git_history
                    .iter()
                    .map(|line| text(line).size(fs.tiny).into())
                    .collect();
                scrollable(Column::with_children(history_items).spacing(1))
                    .height(Length::Fixed(80.0))
            })
            .padding(5)
            .style(container::bordered_box)
            .into()
        };

        // Prominent banner when no baseline — guides the user to snapshot first
        let banner: Element<'_, ProfileManagerMessage> = if !has_baseline {
            container(
                row![
                    text("⚠").size(fs.large),
                    column![
                        text("No baseline captured yet.").size(fs.normal),
                        text(if is_connected {
                            "Click 'Snapshot Baseline' to read the device's current config and schema — required before creating or running profiles."
                        } else {
                            "Connect to the device, then click 'Snapshot Baseline' to capture the current config and schema."
                        })
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.75, 0.75, 0.8)),
                    ]
                    .spacing(3),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .padding(10)
            .style(container::bordered_box)
            .into()
        } else {
            Space::new().height(0).into()
        };

        column![
            toolbar,
            rule::horizontal(1),
            banner,
            profile_list,
            history_panel,
        ]
        .spacing(3)
        .height(Length::Fill)
        .into()
    }

    // ── Profile Editor View ──

    fn view_profile_editor(
        &self,
        is_connected: bool,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
        let profile = match &self.current_profile {
            Some(p) => p,
            None => {
                return container(text("No profile loaded").size(fs.normal))
                    .padding(20)
                    .into();
            }
        };

        // Top toolbar
        let toolbar = row![
            button(text("Back").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::ProfileList))
                .padding([4, 8]),
            rule::vertical(1),
            button(text("Save").size(fs.small))
                .on_press(ProfileManagerMessage::SaveProfile)
                .padding([4, 8])
                .style(if self.is_dirty {
                    button::primary
                } else {
                    button::secondary
                }),
            tooltip(
                button(text("Apply").size(fs.small))
                    .on_press_maybe(
                        (is_connected && self.baseline_config.is_some())
                            .then_some(ProfileManagerMessage::ApplyProfile),
                    )
                    .padding([4, 8])
                    .style(button::success),
                if self.baseline_config.is_none() {
                    "Snapshot the baseline first"
                } else if !is_connected {
                    "Not connected to device"
                } else {
                    "Compute diff against baseline and apply to device"
                },
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            rule::vertical(1),
            button(text("Export .cfg").size(fs.small))
                .on_press(ProfileManagerMessage::ExportCfg)
                .padding([4, 8]),
            button(text("Export .json").size(fs.small))
                .on_press(ProfileManagerMessage::ExportJson)
                .padding([4, 8]),
            rule::vertical(1),
            button(text("Raw JSON").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::RawJson))
                .padding([4, 8]),
            button(text("CFG View").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::RenderedCfg))
                .padding([4, 8]),
            button(text("Diff").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::DiffView))
                .padding([4, 8]),
            rule::vertical(1),
            tooltip(
                button(text("Screenshot").size(fs.small))
                    .on_press_maybe(
                        is_connected.then_some(ProfileManagerMessage::CaptureScreenshot)
                    )
                    .padding([4, 8]),
                "Capture current C64 screen as profile thumbnail (requires streaming)",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("Pick Image").size(fs.small))
                    .on_press(ProfileManagerMessage::PickImage)
                    .padding([4, 8]),
                "Pick an image file (PNG/JPG/etc.) as the profile thumbnail",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            Space::new().width(Length::Fill),
            if self.is_dirty {
                text("* Modified")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.9, 0.7, 0.0))
            } else {
                text("Saved").size(fs.small)
            },
            Space::new().width(10),
            button(text("Delete").size(fs.small))
                .on_press(ProfileManagerMessage::DeleteProfile)
                .padding([4, 8])
                .style(button::danger),
        ]
        .spacing(5)
        .height(Length::Shrink)
        .align_y(iced::Alignment::Center);

        // Left sidebar: profile info + category nav
        let left_sidebar = self.view_left_sidebar(profile, fs);

        // Center: config key/value editor
        let center_panel = self.view_config_editor(profile, fs);

        // Right sidebar: mounts + launch + baseline
        let right_sidebar = self.view_right_sidebar(profile, fs);

        column![
            toolbar,
            rule::horizontal(1),
            row![
                left_sidebar,
                rule::vertical(1),
                center_panel,
                rule::vertical(1),
                right_sidebar,
            ]
            .height(Length::Fill),
        ]
        .spacing(5)
        .height(Length::Fill)
        .into()
    }

    /// Left sidebar: compact profile info at top, then scrollable category list.
    fn view_left_sidebar<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        // Compact profile info header
        let info_header = container(
            column![
                text_input("Profile name", &profile.name)
                    .on_input(ProfileManagerMessage::EditName)
                    .size(fs.normal as f32),
                text_input("Description", &profile.description)
                    .on_input(ProfileManagerMessage::EditDescription)
                    .size(fs.small as f32),
                row![
                    tooltip(
                        text_input("category", &self.save_category)
                            .on_input(ProfileManagerMessage::EditCategory)
                            .size(fs.small as f32)
                            .width(Length::Fill),
                        "Repository folder (e.g. games, demos, music).\nUsed to organize profiles on disk.",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        pick_list(
                            vec!["Full".to_string(), "Overlay".to_string()],
                            Some(profile.profile_mode.to_string()),
                            ProfileManagerMessage::SetProfileMode,
                        )
                        .text_size(fs.small)
                        .width(Length::Fixed(80.0)),
                        "Full: complete snapshot of all device settings.\nOverlay: only settings that override the baseline.\n\nOverlay is preferred for per-game/per-demo profiles —\nsmaller, safer, won't touch unrelated settings.",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(3),
                text_input("tags (comma-separated)", &self.tags_input)
                    .on_input(ProfileManagerMessage::EditTagsInput)
                    .size(fs.small as f32),
                text(format!(
                    "{} | {} cat | {} settings",
                    profile.source_format,
                    profile.config.len(),
                    profile.setting_count()
                ))
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.5, 0.5, 0.55)),
            ]
            .spacing(4),
        )
        .padding(8);

        // Categories list (scrollable, fills remaining height)
        let cat_items: Vec<Element<'_, ProfileManagerMessage>> = profile
            .categories()
            .iter()
            .map(|cat| {
                let is_selected = self.selected_config_category.as_ref() == Some(*cat);
                let item_count = profile.config.get(*cat).map(|m| m.len()).unwrap_or(0);

                button(text(format!("{} ({})", cat, item_count)).size(fs.small))
                    .on_press(ProfileManagerMessage::SelectConfigCategory(cat.to_string()))
                    .padding([3, 6])
                    .width(Length::Fill)
                    .style(if is_selected {
                        button::primary
                    } else {
                        button::text
                    })
                    .into()
            })
            .collect();

        let new_cat_val = self.new_category_input.clone();
        let categories_list = container(
            column![
                text("CATEGORIES").size(fs.tiny),
                scrollable(
                    Column::with_children(cat_items)
                        .spacing(1)
                        .padding(iced::Padding::new(2.0).right(10.0)),
                )
                .height(Length::Fill),
                row![
                    text_input("Add category...", &self.new_category_input)
                        .on_input(ProfileManagerMessage::NewCategoryInput)
                        .on_submit(ProfileManagerMessage::AddCategory(new_cat_val.clone()))
                        .size(fs.small as f32)
                        .width(Length::Fill),
                    button(text("+").size(fs.tiny))
                        .on_press(ProfileManagerMessage::AddCategory(new_cat_val))
                        .padding([2, 6]),
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(3),
        )
        .padding(8);

        container(
            column![info_header, rule::horizontal(1), categories_list,]
                .spacing(0)
                .height(Length::Fill),
        )
        .width(Length::Fixed(220.0))
        .into()
    }

    /// Right sidebar: media mounts + launch settings + baseline.
    fn view_right_sidebar<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        // Mounts
        let mounts_section = self.view_mounts_section(profile, fs);

        // Launch
        let launch_section = container(
            column![
                text("LAUNCH").size(fs.tiny),
                row![
                    toggler(profile.launch.restore_baseline_first)
                        .on_toggle(ProfileManagerMessage::RestoreBaselineChanged)
                        .size(fs.normal),
                    text("Restore baseline first").size(fs.small),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
                row![
                    toggler(profile.launch.reset_after_apply)
                        .on_toggle(ProfileManagerMessage::ResetAfterApplyChanged)
                        .size(fs.normal),
                    text("Reset after apply").size(fs.small),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(4),
        )
        .padding(8);

        // Baseline status
        let has_schema = self.config_schema.is_some();
        let baseline_info = if let Some(bl) = &self.baseline_config {
            let count: usize = bl.values().map(|v| v.len()).sum();
            format!("{} cat, {} settings", bl.len(), count)
        } else {
            "Not captured".to_string()
        };

        let mut baseline_col = column![
            text("BASELINE").size(fs.tiny),
            button(text("Snapshot Baseline").size(fs.small))
                .on_press(ProfileManagerMessage::SnapshotBaseline)
                .padding([3, 6])
                .width(Length::Fill)
                .style(crate::styles::baseline_button),
            if self.baseline_config.is_some() {
                text(baseline_info)
                    .size(fs.tiny)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3))
            } else {
                text(baseline_info)
                    .size(fs.tiny)
                    .color(iced::Color::from_rgb(0.9, 0.5, 0.3))
            },
        ]
        .spacing(4);

        // Warn if baseline is captured but schema is missing (old baseline file)
        if self.baseline_config.is_some() && !has_schema {
            baseline_col = baseline_col.push(
                text("⚠ No schema — re-snapshot for dropdowns")
                    .size(fs.tiny)
                    .color(iced::Color::from_rgb(0.95, 0.6, 0.3)),
            );
        }

        let baseline_section = container(baseline_col).padding(8);

        container(
            scrollable(
                column![
                    mounts_section,
                    rule::horizontal(1),
                    launch_section,
                    rule::horizontal(1),
                    baseline_section,
                ]
                .spacing(0),
            )
            .height(Length::Fill),
        )
        .width(Length::Fixed(260.0))
        .into()
    }

    fn view_mounts_section<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let drive_a_path = profile
            .mounts
            .drive_a
            .as_ref()
            .map(|m| m.path.as_str())
            .unwrap_or("");
        let drive_a_autoload = profile
            .mounts
            .drive_a
            .as_ref()
            .map(|m| m.autoload)
            .unwrap_or(false);

        let drive_b_path = profile
            .mounts
            .drive_b
            .as_ref()
            .map(|m| m.path.as_str())
            .unwrap_or("");
        let drive_b_autoload = profile
            .mounts
            .drive_b
            .as_ref()
            .map(|m| m.autoload)
            .unwrap_or(false);

        let cart_path = profile
            .mounts
            .cartridge
            .as_ref()
            .map(|m| m.path.as_str())
            .unwrap_or("");

        container(
            column![
                tooltip(
                    text("MEDIA MOUNTS").size(fs.tiny),
                    "Local files are uploaded to the device.\nDevice paths (e.g. /Usb0/...) mount directly.",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                // Drive A
                text("Drive A").size(fs.tiny),
                row![
                    text_input("disk image path...", drive_a_path)
                        .on_input(ProfileManagerMessage::DriveAPathChanged)
                        .size(fs.small as f32)
                        .width(Length::Fill),
                    button(text("..").size(fs.tiny))
                        .on_press(ProfileManagerMessage::BrowseDriveA)
                        .padding([2, 5]),
                    button(text("x").size(fs.tiny))
                        .on_press(ProfileManagerMessage::ClearDriveA)
                        .padding([2, 5]),
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
                row![
                    toggler(drive_a_autoload)
                        .on_toggle(ProfileManagerMessage::DriveAAutoloadChanged)
                        .size(fs.normal),
                    tooltip(
                        text("Run").size(fs.tiny),
                        "Enable drive, mount, reset, LOAD\"*\",8,1 and RUN",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center),
                // Drive B
                text("Drive B").size(fs.tiny),
                row![
                    text_input("disk image path...", drive_b_path)
                        .on_input(ProfileManagerMessage::DriveBPathChanged)
                        .size(fs.small as f32)
                        .width(Length::Fill),
                    button(text("..").size(fs.tiny))
                        .on_press(ProfileManagerMessage::BrowseDriveB)
                        .padding([2, 5]),
                    button(text("x").size(fs.tiny))
                        .on_press(ProfileManagerMessage::ClearDriveB)
                        .padding([2, 5]),
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
                row![
                    toggler(drive_b_autoload)
                        .on_toggle(ProfileManagerMessage::DriveBAutoloadChanged)
                        .size(fs.normal),
                    tooltip(
                        text("Run").size(fs.tiny),
                        "Enable drive, mount, reset, LOAD\"*\",9,1 and RUN",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center),
                // Cartridge / program (accepts .crt, .prg, .sid)
                tooltip(
                    text("Cart / Program").size(fs.tiny),
                    "Accepts .crt (cartridge), .prg (program), or .sid (music).\nLocal files are uploaded; device paths run directly.",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                row![
                    text_input(".crt / .prg / .sid path...", cart_path)
                        .on_input(ProfileManagerMessage::CartridgePathChanged)
                        .size(fs.small as f32)
                        .width(Length::Fill),
                    button(text("..").size(fs.tiny))
                        .on_press(ProfileManagerMessage::BrowseCartridge)
                        .padding([2, 5]),
                    button(text("x").size(fs.tiny))
                        .on_press(ProfileManagerMessage::ClearCartridge)
                        .padding([2, 5]),
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(4),
        )
        .padding(8)
        .into()
    }

    // ── Config Editor (center panel) ──
    // Reuses patterns from config_editor.rs

    fn view_config_editor<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let header = container(
            column![row![
                text(
                    self.selected_config_category
                        .as_deref()
                        .unwrap_or("Add Categories")
                )
                .size(fs.large),
                Space::new().width(Length::Fill),
                text("Filter:").size(fs.small),
                text_input("filter...", &self.search_filter)
                    .on_input(ProfileManagerMessage::SearchChanged)
                    .size(fs.normal as f32)
                    .width(Length::Fixed(120.0)),
            ]
            .spacing(5)
            .align_y(iced::Alignment::Center),]
            .spacing(5),
        )
        .padding(10);

        let body: Element<'_, ProfileManagerMessage> = match &self.selected_config_category {
            Some(cat_name) => self.view_category_contents(profile, cat_name, fs),
            None => self.view_category_picker(profile, fs),
        };

        container(
            column![header, rule::horizontal(1), body]
                .spacing(0)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .into()
    }

    /// Show the contents of a selected category: unified list of all baseline
    /// keys, each either editable (in profile) or addable (with + button).
    fn view_category_contents<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        cat_name: &'a str,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let filter_lower = self.search_filter.to_lowercase();
        static EMPTY_MAP: std::sync::OnceLock<HashMap<String, serde_json::Value>> =
            std::sync::OnceLock::new();
        let empty_map = EMPTY_MAP.get_or_init(HashMap::new);
        let items = profile.config.get(cat_name).unwrap_or(empty_map);
        let diff_items = self.get_diff_for_category(cat_name);

        // Build the full key list from the baseline (borrows of 'a — long enough
        // to return Elements that reference them)
        let all_keys: Vec<&'a String> = if let Some(baseline) = &self.baseline_config {
            if let Some(base_items) = baseline.get(cat_name) {
                let mut keys: Vec<&String> = base_items.keys().collect();
                keys.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
                keys
            } else {
                let mut keys: Vec<&String> = items.keys().collect();
                keys.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
                keys
            }
        } else {
            let mut keys: Vec<&String> = items.keys().collect();
            keys.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
            keys
        };

        let filtered_keys: Vec<&'a String> = all_keys
            .into_iter()
            .filter(|k| filter_lower.is_empty() || k.to_lowercase().contains(&filter_lower))
            .collect();

        let in_profile_count = filtered_keys
            .iter()
            .filter(|k| items.contains_key(k.as_str()))
            .count();
        let total_count = filtered_keys.len();

        let mut item_views: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();

        // Top toolbar: Back + Add All + counts
        let toolbar = row![
            button(
                row![
                    text("←").size(fs.normal),
                    text("All Categories").size(fs.small),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .on_press(ProfileManagerMessage::DeselectConfigCategory)
            .padding([4, 10])
            .style(button::text),
            Space::new().width(Length::Fill),
            text(format!("{} / {} in profile", in_profile_count, total_count))
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
            Space::new().width(10),
            tooltip(
                button(text("+ Add All").size(fs.small))
                    .on_press(ProfileManagerMessage::AddAllKeysFromSchema(
                        cat_name.to_string(),
                    ))
                    .padding([4, 10])
                    .style(button::primary),
                "Add every key from the baseline to this category",
                tooltip::Position::Left,
            )
            .style(container::bordered_box),
        ]
        .spacing(5)
        .height(Length::Shrink)
        .align_y(iced::Alignment::Center);

        item_views.push(container(toolbar).padding([4, 8]).into());

        // Empty state
        if filtered_keys.is_empty() {
            if self.baseline_config.is_none() {
                item_views.push(
                    container(
                        text("No baseline captured.\nClick 'Snapshot Baseline' to populate available keys.")
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                    )
                    .padding(20)
                    .into(),
                );
            } else {
                item_views.push(
                    container(
                        text("No keys match the filter.")
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                    )
                    .padding(20)
                    .into(),
                );
            }
        }

        // Unified rendering — each key is either in profile (editable) or available (add)
        for key_ref in filtered_keys.iter() {
            let key_str: &'a str = key_ref.as_str();
            if let Some(value) = items.get(key_str) {
                // In profile — render editor + remove button
                let is_diff = diff_items
                    .as_ref()
                    .map(|d| d.contains_key(key_str))
                    .unwrap_or(false);
                let item = self.view_config_item(cat_name, key_str, value, is_diff, fs);
                let cat = cat_name.to_string();
                let k = key_str.to_string();
                item_views.push(
                    row![
                        item,
                        button(text("x").size(fs.tiny))
                            .on_press(ProfileManagerMessage::RemoveKey(cat, k))
                            .padding([2, 6])
                            .style(button::danger),
                    ]
                    .spacing(4)
                    .align_y(iced::Alignment::Center)
                    .into(),
                );
            } else {
                // Not in profile — render a single-line "Add" row with baseline hint
                let hint = self
                    .baseline_config
                    .as_ref()
                    .and_then(|b| b.get(cat_name))
                    .and_then(|c| c.get(key_str))
                    .map(|v| config_api::format_value(v))
                    .unwrap_or_else(|| "—".to_string());
                let cat = cat_name.to_string();
                let k = key_str.to_string();
                item_views.push(
                    container(
                        row![
                            text(key_str)
                                .size(fs.small)
                                .color(iced::Color::from_rgb(0.65, 0.65, 0.7))
                                .width(Length::Fill),
                            text(format!("baseline: {}", hint))
                                .size(fs.tiny)
                                .color(iced::Color::from_rgb(0.5, 0.5, 0.55)),
                            button(text("+").size(fs.small))
                                .on_press(ProfileManagerMessage::AddKeyFromSchema(cat, k))
                                .padding([2, 10]),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding([4, 10])
                    .style(container::bordered_box)
                    .into(),
                );
            }
        }

        scrollable(
            Column::with_children(item_views)
                .spacing(6)
                .padding(iced::Padding::new(10.0).right(15.0)),
        )
        .height(Length::Fill)
        .into()
    }

    /// Show a picker to add categories to the profile — popular shortcuts
    /// at the top, then the full list of categories from the baseline.
    fn view_category_picker<'a>(
        &'a self,
        profile: &'a DeviceProfile,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let filter_lower = self.search_filter.to_lowercase();

        let available_cats: Vec<&String> = if let Some(baseline) = &self.baseline_config {
            let mut v: Vec<&String> = baseline.keys().collect();
            v.sort();
            v
        } else {
            Vec::new()
        };

        let mut sections: Vec<Element<'_, ProfileManagerMessage>> = Vec::new();

        // Intro text
        sections.push(
            container(
                column![
                    text("Click a category to add it to the profile.")
                        .size(fs.normal)
                        .color(iced::Color::from_rgb(0.75, 0.75, 0.8)),
                    text("Added categories get populated with baseline values — edit or delete keys as needed.")
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
                ]
                .spacing(3),
            )
            .padding([8, 10])
            .into(),
        );

        // Popular section
        let popular_available: Vec<&&str> = POPULAR_CATEGORIES
            .iter()
            .filter(|name| {
                !profile.config.contains_key(**name)
                    && (filter_lower.is_empty() || name.to_lowercase().contains(&filter_lower))
                    && (available_cats.is_empty()
                        || available_cats.iter().any(|c| c.as_str() == **name))
            })
            .collect();

        if !popular_available.is_empty() {
            sections.push(
                container(
                    text("POPULAR")
                        .size(fs.tiny)
                        .color(iced::Color::from_rgb(0.5, 0.6, 0.75)),
                )
                .padding([8, 10])
                .into(),
            );
            for name in popular_available {
                let name_str = name.to_string();
                sections.push(
                    container(
                        button(
                            row![
                                text("★")
                                    .size(fs.normal)
                                    .color(iced::Color::from_rgb(0.9, 0.7, 0.2)),
                                text(*name).size(fs.normal).width(Length::Fill),
                                text("Add")
                                    .size(fs.tiny)
                                    .color(iced::Color::from_rgb(0.5, 0.8, 0.5)),
                            ]
                            .spacing(8)
                            .align_y(iced::Alignment::Center),
                        )
                        .on_press(ProfileManagerMessage::AddCategoryFromSchema(name_str))
                        .padding([6, 10])
                        .width(Length::Fill)
                        .style(button::text),
                    )
                    .into(),
                );
            }
        }

        // All baseline categories section (filtered)
        let other_cats: Vec<&String> = available_cats
            .iter()
            .filter(|name| {
                !profile.config.contains_key(name.as_str())
                    && !POPULAR_CATEGORIES.contains(&name.as_str())
                    && (filter_lower.is_empty() || name.to_lowercase().contains(&filter_lower))
            })
            .copied()
            .collect();

        if !other_cats.is_empty() {
            sections.push(
                container(
                    text("ALL CATEGORIES")
                        .size(fs.tiny)
                        .color(iced::Color::from_rgb(0.5, 0.6, 0.75)),
                )
                .padding([12, 10])
                .into(),
            );
            for name in other_cats {
                let name_str = name.clone();
                let item_count = self
                    .baseline_config
                    .as_ref()
                    .and_then(|b| b.get(name))
                    .map(|c| c.len())
                    .unwrap_or(0);
                sections.push(
                    container(
                        button(
                            row![
                                text(name).size(fs.normal).width(Length::Fill),
                                text(format!("{} keys", item_count))
                                    .size(fs.tiny)
                                    .color(iced::Color::from_rgb(0.5, 0.5, 0.55)),
                                text("Add")
                                    .size(fs.tiny)
                                    .color(iced::Color::from_rgb(0.5, 0.8, 0.5)),
                            ]
                            .spacing(8)
                            .align_y(iced::Alignment::Center),
                        )
                        .on_press(ProfileManagerMessage::AddCategoryFromSchema(name_str))
                        .padding([6, 10])
                        .width(Length::Fill)
                        .style(button::text),
                    )
                    .into(),
                );
            }
        }

        // No baseline? Tell the user
        if available_cats.is_empty() {
            sections.push(
                container(
                    column![
                        text("No baseline captured yet.")
                            .size(fs.normal)
                            .color(iced::Color::from_rgb(0.9, 0.6, 0.4)),
                        text("Click 'Snapshot Baseline' to read the device's")
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                        text("current configuration — this populates the list of")
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                        text("available categories and keys for profile editing.")
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                    ]
                    .spacing(3),
                )
                .padding(20)
                .into(),
            );
        }

        scrollable(
            Column::with_children(sections)
                .spacing(2)
                .padding(iced::Padding::new(4.0).right(15.0)),
        )
        .height(Length::Fill)
        .into()
    }

    /// Look up the schema for a specific config key.
    fn get_item_schema(
        &self,
        category: &str,
        key: &str,
    ) -> Option<&crate::device_profile::ItemSchema> {
        self.config_schema.as_ref()?.get(category)?.get(key)
    }

    /// Heuristic: does this config key look like a device filesystem path?
    /// Checks the key name for common path-related words, or the current value
    /// for a leading slash (typical of device paths like /Usb0/... or /Flash/...).
    fn is_path_field(key: &str, value: &serde_json::Value) -> bool {
        let lower = key.to_lowercase();
        let name_hints = lower.contains("path")
            || lower.contains("image")
            || lower.contains("file")
            || lower.contains("dir")
            || lower.contains("directory");
        if name_hints {
            return true;
        }
        // Fall back to checking if the value looks like an absolute device path
        if let Some(s) = value.as_str() {
            let trimmed = s.trim_start();
            return trimmed.starts_with('/');
        }
        false
    }

    /// Render a single config key/value editor. If the baseline captured a
    /// schema for this key, the widget uses it (pick_list for enums, slider
    /// for integers). Otherwise falls back to a heuristic text/toggle widget.
    fn view_config_item<'a>(
        &'a self,
        category: &'a str,
        key: &'a str,
        value: &'a serde_json::Value,
        is_diff: bool,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let cat = category.to_string();
        let k = key.to_string();
        let item_schema = self.get_item_schema(category, key);

        let name_color = if is_diff && self.show_diff {
            iced::Color::from_rgb(0.9, 0.7, 0.0)
        } else {
            iced::Color::WHITE
        };

        let name_row =
            row![text(key).size(fs.normal).color(name_color),].align_y(iced::Alignment::Center);

        // Schema-driven widget selection
        let control: Element<'_, ProfileManagerMessage> = if let Some(schema) = item_schema {
            if let Some(options) = schema.options.as_ref().filter(|o| !o.is_empty()) {
                // Enum — dropdown with exact API-valid values
                let current_str = match value {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                };
                let cat2 = cat.clone();
                let k2 = k.clone();
                pick_list(options.clone(), current_str, move |v| {
                    ProfileManagerMessage::ConfigValueChanged(
                        cat2.clone(),
                        k2.clone(),
                        serde_json::Value::String(v),
                    )
                })
                .text_size(fs.normal)
                .width(Length::Fixed(260.0))
                .into()
            } else if let (Some(min), Some(max)) = (schema.min, schema.max) {
                // Integer with range — slider
                let current = match value {
                    serde_json::Value::Number(n) => n.as_i64().unwrap_or(min),
                    serde_json::Value::String(s) => s.trim().parse::<i64>().unwrap_or(min),
                    _ => min,
                };
                let unit = schema
                    .format
                    .as_deref()
                    .map(|f| {
                        if f.contains("dB") {
                            " dB"
                        } else if f.ends_with('%') {
                            "%"
                        } else if f.contains("ppm") {
                            " ppm"
                        } else {
                            ""
                        }
                    })
                    .unwrap_or("");
                let cat2 = cat.clone();
                let k2 = k.clone();
                row![
                    iced::widget::slider(min as f64..=max as f64, current as f64, move |v| {
                        ProfileManagerMessage::ConfigValueChanged(
                            cat2.clone(),
                            k2.clone(),
                            serde_json::json!(v as i64),
                        )
                    })
                    .step(1.0)
                    .width(Length::Fixed(160.0)),
                    Space::new().width(8),
                    text(format!("{}{}", current, unit)).size(fs.normal),
                    Space::new().width(5),
                    text(format!("[{}..{}]", min, max))
                        .size(fs.tiny)
                        .color(iced::Color::from_rgb(0.5, 0.5, 0.5)),
                ]
                .spacing(3)
                .align_y(iced::Alignment::Center)
                .into()
            } else if let Some(presets) = schema.presets.as_ref().filter(|p| !p.is_empty()) {
                // Presets: text input + preset dropdown + Browse button.
                // Unlike enums, the user CAN type a custom path.
                let current_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    _ => config_api::format_value(value),
                };
                let cat2 = cat.clone();
                let k2 = k.clone();
                let cat3 = cat.clone();
                let k3 = k.clone();
                let cat4 = cat.clone();
                let k4 = k.clone();
                let current_for_browse = current_str.clone();
                row![
                    text_input("path or filename...", &current_str)
                        .on_input(move |v| {
                            ProfileManagerMessage::ConfigValueChanged(
                                cat2.clone(),
                                k2.clone(),
                                serde_json::Value::String(v),
                            )
                        })
                        .size(fs.normal as f32)
                        .width(Length::Fill),
                    pick_list(
                        presets.clone(),
                        if presets.contains(&current_str) {
                            Some(current_str)
                        } else {
                            None
                        },
                        move |v| {
                            ProfileManagerMessage::ConfigValueChanged(
                                cat3.clone(),
                                k3.clone(),
                                serde_json::Value::String(v),
                            )
                        },
                    )
                    .text_size(fs.small)
                    .width(Length::Fixed(100.0)),
                    button(text("Browse…").size(fs.tiny))
                        .on_press(ProfileManagerMessage::OpenRemotePicker(
                            cat4,
                            k4,
                            current_for_browse,
                        ))
                        .padding([2, 8]),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                self.view_config_item_fallback(&cat, &k, value, fs)
            }
        } else {
            self.view_config_item_fallback(&cat, &k, value, fs)
        };

        // Track whether the schema-driven control already includes a Browse button
        // (presets have one built-in). If so, skip the is_path_field wrapper.
        let has_browse_already = item_schema
            .map(|s| s.presets.as_ref().map_or(false, |p| !p.is_empty()))
            .unwrap_or(false);

        // Show baseline diff value if available
        let diff_hint: Element<'_, ProfileManagerMessage> = if is_diff && self.show_diff {
            if let Some(baseline) = &self.baseline_config {
                if let Some(base_val) = baseline.get(category).and_then(|c| c.get(key)) {
                    text(format!("baseline: {}", config_api::format_value(base_val)))
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.5, 0.7, 0.5))
                        .into()
                } else {
                    text("(new key)")
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.5, 0.7, 0.5))
                        .into()
                }
            } else {
                Space::new().height(0).into()
            }
        } else {
            Space::new().height(0).into()
        };

        // Add an FTP Browse button for path-like fields (unless the schema
        // already provided one via the presets branch)
        let control_row: Element<'_, ProfileManagerMessage> =
            if !has_browse_already && Self::is_path_field(key, value) {
                let current_path = value.as_str().unwrap_or("").to_string();
                let cat_b = cat.clone();
                let k_b = k.clone();
                row![
                    control,
                    tooltip(
                        button(text("Browse…").size(fs.tiny))
                            .on_press(ProfileManagerMessage::OpenRemotePicker(
                                cat_b,
                                k_b,
                                current_path,
                            ))
                            .padding([2, 8]),
                        "Browse the device filesystem via FTP",
                        tooltip::Position::Left,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                control
            };

        container(column![name_row, control_row, diff_hint].spacing(3))
            .padding([8, 10])
            .width(Length::Fill)
            .style(container::bordered_box)
            .into()
    }

    /// Fallback widget when no schema is available — heuristic based on value type.
    fn view_config_item_fallback<'a>(
        &'a self,
        cat: &str,
        k: &str,
        value: &'a serde_json::Value,
        fs: crate::styles::FontSizes,
    ) -> Element<'a, ProfileManagerMessage> {
        let cat = cat.to_string();
        let k = k.to_string();

        match value {
            serde_json::Value::Number(n) => {
                let current = n.as_i64().unwrap_or(0);
                let cat2 = cat.clone();
                let k2 = k.clone();
                text_input("", &current.to_string())
                    .on_input(move |v| {
                        let parsed = v.parse::<i64>().unwrap_or(current);
                        ProfileManagerMessage::ConfigValueChanged(
                            cat2.clone(),
                            k2.clone(),
                            serde_json::json!(parsed),
                        )
                    })
                    .size(fs.normal as f32)
                    .width(Length::Fixed(120.0))
                    .into()
            }
            _ => {
                let current_str: &str = match value {
                    serde_json::Value::String(s) => s.as_str(),
                    serde_json::Value::Null => "",
                    _ => "",
                };
                let formatted;
                let display_str = if current_str.is_empty()
                    && !matches!(
                        value,
                        serde_json::Value::String(_) | serde_json::Value::Null
                    ) {
                    formatted = value.to_string();
                    formatted.as_str()
                } else {
                    current_str
                };

                let lower = display_str.to_lowercase();
                let bool_pair: Option<(&str, &str)> = match lower.as_str() {
                    "enabled" | "disabled" => Some(("Enabled", "Disabled")),
                    "yes" | "no" => Some(("Yes", "No")),
                    "on" | "off" => Some(("On", "Off")),
                    "true" | "false" => Some(("true", "false")),
                    _ => None,
                };
                if let Some((on_val, off_val)) = bool_pair {
                    let is_on = matches!(lower.as_str(), "enabled" | "yes" | "on" | "true");
                    let cat2 = cat.clone();
                    let k2 = k.clone();
                    let on_s = on_val.to_string();
                    let off_s = off_val.to_string();
                    row![
                        toggler(is_on)
                            .on_toggle(move |v| {
                                let str_val = if v { on_s.clone() } else { off_s.clone() };
                                ProfileManagerMessage::ConfigValueChanged(
                                    cat2.clone(),
                                    k2.clone(),
                                    serde_json::Value::String(str_val),
                                )
                            })
                            .size(fs.header),
                        text(display_str.to_string()).size(fs.normal),
                    ]
                    .spacing(5)
                    .align_y(iced::Alignment::Center)
                    .into()
                } else {
                    let cat2 = cat.clone();
                    let k2 = k.clone();
                    text_input("", display_str)
                        .on_input(move |v| {
                            ProfileManagerMessage::ConfigValueChanged(
                                cat2.clone(),
                                k2.clone(),
                                serde_json::Value::String(v),
                            )
                        })
                        .size(fs.normal as f32)
                        .width(Length::Fixed(250.0))
                        .into()
                }
            }
        }
    }

    // ── Diff View ──

    fn view_diff(&self, fs: crate::styles::FontSizes) -> Element<'_, ProfileManagerMessage> {
        let toolbar = row![
            button(text("Back to Editor").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::ProfileEditor))
                .padding([4, 8]),
            Space::new().width(10),
            button(text("Snapshot Baseline").size(fs.small))
                .on_press(ProfileManagerMessage::SnapshotBaseline)
                .padding([4, 8])
                .style(crate::styles::baseline_button),
        ]
        .spacing(5);

        let diff_content: Element<'_, ProfileManagerMessage> =
            match (&self.current_profile, &self.baseline_config) {
                (Some(profile), Some(baseline)) => {
                    let diff = device_profile::diff_configs(baseline, &profile.config);
                    if diff.is_empty() {
                        container(text("No differences from baseline").size(fs.normal))
                            .padding(20)
                            .into()
                    } else {
                        let total_diffs: usize = diff.values().map(|v| v.len()).sum();
                        let mut items: Vec<Element<'_, ProfileManagerMessage>> =
                            vec![text(format!(
                                "{} differences across {} categories",
                                total_diffs,
                                diff.len()
                            ))
                            .size(fs.normal)
                            .into()];

                        let mut categories: Vec<_> = diff.keys().collect();
                        categories.sort();

                        for cat in categories {
                            items.push(
                                text(format!("[{}]", cat))
                                    .size(fs.normal)
                                    .color(iced::Color::from_rgb(0.4, 0.7, 1.0))
                                    .into(),
                            );
                            if let Some(cat_items) = diff.get(cat) {
                                let mut keys: Vec<_> = cat_items.keys().collect();
                                keys.sort();
                                for key in keys {
                                    let new_val = cat_items.get(key).unwrap();
                                    let old_val = baseline.get(cat).and_then(|c| c.get(key));

                                    let diff_line = if let Some(old) = old_val {
                                        format!(
                                            "  {} : {} -> {}",
                                            key,
                                            config_api::format_value(old),
                                            config_api::format_value(new_val)
                                        )
                                    } else {
                                        format!(
                                            "  {} : (new) {}",
                                            key,
                                            config_api::format_value(new_val)
                                        )
                                    };

                                    items.push(
                                        text(diff_line)
                                            .size(fs.small)
                                            .color(iced::Color::from_rgb(0.9, 0.7, 0.0))
                                            .into(),
                                    );
                                }
                            }
                        }

                        scrollable(Column::with_children(items).spacing(4).padding(10))
                            .height(Length::Fill)
                            .into()
                    }
                }
                (Some(_), None) => {
                    container(text("Load a baseline first to see differences").size(fs.normal))
                        .padding(20)
                        .into()
                }
                _ => container(text("No profile loaded").size(fs.normal))
                    .padding(20)
                    .into(),
            };

        column![toolbar, rule::horizontal(1), diff_content]
            .spacing(5)
            .height(Length::Fill)
            .into()
    }

    // ── Raw JSON View ──

    fn view_raw_json(&self, fs: crate::styles::FontSizes) -> Element<'_, ProfileManagerMessage> {
        let toolbar = row![button(text("Back to Editor").size(fs.small))
            .on_press(ProfileManagerMessage::SwitchView(ViewMode::ProfileEditor))
            .padding([4, 8]),];

        let json_content: Element<'_, ProfileManagerMessage> =
            if let Some(profile) = &self.current_profile {
                let json_str = serde_json::to_string_pretty(profile).unwrap_or_default();
                scrollable(container(text(json_str).size(fs.small)).padding(10))
                    .height(Length::Fill)
                    .into()
            } else {
                text("No profile loaded").size(fs.normal).into()
            };

        column![toolbar, rule::horizontal(1), json_content]
            .spacing(5)
            .height(Length::Fill)
            .into()
    }

    // ── Rendered CFG View ──

    fn view_rendered_cfg(
        &self,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
        let toolbar = row![
            button(text("Back to Editor").size(fs.small))
                .on_press(ProfileManagerMessage::SwitchView(ViewMode::ProfileEditor))
                .padding([4, 8]),
            Space::new().width(10),
            button(text("Export .cfg").size(fs.small))
                .on_press(ProfileManagerMessage::ExportCfg)
                .padding([4, 8]),
        ]
        .spacing(5);

        let cfg_content: Element<'_, ProfileManagerMessage> =
            if let Some(profile) = &self.current_profile {
                let cfg_str = cfg_format::export_profile_cfg(profile);
                scrollable(container(text(cfg_str).size(fs.small)).padding(10))
                    .height(Length::Fill)
                    .into()
            } else {
                text("No profile loaded").size(fs.normal).into()
            };

        column![toolbar, rule::horizontal(1), cfg_content]
            .spacing(5)
            .height(Length::Fill)
            .into()
    }

    // ── Status Bar ──

    fn view_status_bar(&self, fs: crate::styles::FontSizes) -> Element<'_, ProfileManagerMessage> {
        let baseline_status = if let Some(bl) = &self.baseline_config {
            let count: usize = bl.values().map(|v| v.len()).sum();
            format!("Baseline: {} cat, {} settings", bl.len(), count)
        } else {
            "No baseline".to_string()
        };

        container(
            row![
                if let Some(err) = &self.error_message {
                    text(err)
                        .size(fs.normal)
                        .color(iced::Color::from_rgb(0.9, 0.3, 0.3))
                } else if let Some(status) = &self.status_message {
                    text(status).size(fs.normal)
                } else {
                    text("").size(fs.normal)
                },
                Space::new().width(Length::Fill),
                text(baseline_status)
                    .size(fs.small)
                    .color(if self.baseline_config.is_some() {
                        iced::Color::from_rgb(0.4, 0.7, 0.4)
                    } else {
                        iced::Color::from_rgb(0.7, 0.5, 0.3)
                    }),
                Space::new().width(15),
                text(format!("{} profiles", self.profiles.len())).size(fs.normal),
                Space::new().width(10),
                if self.is_loading {
                    text("Loading...").size(fs.normal)
                } else {
                    text("").size(fs.normal)
                },
            ]
            .align_y(iced::Alignment::Center),
        )
        .padding([5, 10])
        .into()
    }

    // ── Helpers ──

    /// Get diff items for a specific category (profile vs baseline).
    fn get_diff_for_category(&self, category: &str) -> Option<HashMap<String, serde_json::Value>> {
        let profile = self.current_profile.as_ref()?;
        let baseline = self.baseline_config.as_ref()?;

        let diff = device_profile::diff_configs(baseline, &profile.config);
        diff.get(category).cloned()
    }
}
