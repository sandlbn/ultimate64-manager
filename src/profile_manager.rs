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
    ConfigValueChanged(String, String, serde_json::Value),
    AddCategory(String),
    RemoveCategory(String),
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
    ApplyProfileFromList(usize),
    ApplyProfileComplete(Result<String, String>),

    // Screenshot capture from streaming frame buffer
    CaptureScreenshot,

    // Baseline — captured once from device, stored in repo, loaded at startup
    SnapshotBaseline,
    BaselineSnapshotted(Result<(ConfigTree, String), String>),
    BaselineLoadedFromDisk(Option<ConfigTree>),
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

    // Baseline for diff view
    baseline_config: Option<ConfigTree>,
    show_diff: bool,

    // UI state
    view_mode: ViewMode,
    is_loading: bool,
    status_message: Option<String>,
    error_message: Option<String>,
    git_history: Vec<String>,
}

impl ProfileManager {
    pub fn new() -> Self {
        let repo_root = ProfileRepo::default_path().unwrap_or_else(|| PathBuf::from("."));
        let repo_initialized = repo_root.join(".git").is_dir();

        // Try to load stored baseline from repo at startup
        let baseline_config = if repo_initialized {
            let repo = ProfileRepo::new(repo_root.clone());
            match repo.load_baseline("default") {
                Ok(config) => {
                    let count: usize = config.values().map(|v| v.len()).sum();
                    log::info!(
                        "Loaded stored baseline: {} categories, {} settings",
                        config.len(),
                        count
                    );
                    Some(config)
                }
                Err(_) => {
                    log::info!("No stored baseline found — snapshot device to create one");
                    None
                }
            }
        } else {
            None
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
            baseline_config,
            show_diff: false,
            view_mode: ViewMode::ProfileList,
            is_loading: false,
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
                        profile.config.entry(name).or_default();
                        self.is_dirty = true;
                    }
                    self.new_category_input.clear();
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
                        .set_title("Select Cartridge Image")
                        .add_filter("Cartridge images", &["crt"])
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
                            if let Some(png_bytes) = screenshot_data {
                                let cat = if category.is_empty() {
                                    "uncategorized".to_string()
                                } else {
                                    category
                                };
                                let profile_dir =
                                    root.join("profiles").join(&cat).join(&profile.id);
                                let dest = profile_dir.join("screenshot.png");
                                tokio::fs::write(&dest, &png_bytes)
                                    .await
                                    .map_err(|e| format!("Failed to save screenshot: {}", e))?;
                                log::info!("Saved screenshot to {}", dest.display());
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
            ProfileManagerMessage::ApplyProfile => {
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
                            "No baseline stored. Click 'Snapshot Baseline' first to capture the device's current config."
                                .to_string(),
                        );
                        return Task::none();
                    }
                };

                // Compute diff locally — instant, no device reads
                let diff = device_profile::diff_configs(&baseline, &profile.config);
                let diff_count: usize = diff.values().map(|v| v.len()).sum();

                self.is_loading = true;
                self.status_message = Some(format!(
                    "Applying '{}' — {} settings to change...",
                    profile.name, diff_count
                ));

                let conn = connection.clone();
                Task::perform(
                    async move {
                        profile_api::apply_profile(host, &profile, diff, password, conn).await
                    },
                    ProfileManagerMessage::ApplyProfileComplete,
                )
            }
            ProfileManagerMessage::ApplyProfileFromList(index) => {
                // Load profile from repo and apply directly without entering editor
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
                    self.is_loading = true;
                    self.status_message = Some(format!("Running '{}'...", name));

                    let conn = connection.clone();
                    Task::perform(
                        async move {
                            let profile = profile_repo::load_profile_async(path).await?;
                            let diff = device_profile::diff_configs(&baseline, &profile.config);
                            let diff_count: usize = diff.values().map(|v| v.len()).sum();
                            log::info!(
                                "Running '{}' from list: {} settings to change",
                                profile.name,
                                diff_count
                            );
                            profile_api::apply_profile(host, &profile, diff, password, conn).await
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

            // ── Baseline ──
            // Snapshot device config, store as "default" baseline in repo, keep in memory
            ProfileManagerMessage::SnapshotBaseline => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message =
                        Some("Snapshotting device config as baseline (one-time)...".to_string());
                    let root = self.repo_root.clone();
                    Task::perform(
                        async move {
                            // Read all config from device
                            let config = profile_api::read_current_config(host, password).await?;
                            let count: usize = config.values().map(|v| v.len()).sum();

                            // Save to repo
                            tokio::task::spawn_blocking({
                                let config = config.clone();
                                move || {
                                    let mut repo = ProfileRepo::new(root);
                                    repo.save_baseline("default", &config)?;
                                    repo.commit("Snapshot device baseline")?;
                                    Ok::<(), String>(())
                                }
                            })
                            .await
                            .map_err(|e| format!("Task error: {}", e))??;

                            Ok((
                                config,
                                format!("Baseline saved: {} categories, {} settings", count, count),
                            ))
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
                    Ok((config, msg)) => {
                        let count: usize = config.values().map(|v| v.len()).sum();
                        self.status_message = Some(format!(
                            "Baseline stored: {} categories, {} settings",
                            config.len(),
                            count
                        ));
                        self.baseline_config = Some(config);
                        self.error_message = None;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Baseline snapshot failed: {}", e));
                    }
                }
                Task::none()
            }
            ProfileManagerMessage::BaselineLoadedFromDisk(config) => {
                self.baseline_config = config;
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

    // ── Profile List View ──

    fn view_profile_list(
        &self,
        is_connected: bool,
        fs: crate::styles::FontSizes,
    ) -> Element<'_, ProfileManagerMessage> {
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
                .on_press(ProfileManagerMessage::NewProfile)
                .padding([4, 8])
                .into(),
        );
        toolbar_items.push(
            button(text("Import .cfg").size(fs.small))
                .on_press(ProfileManagerMessage::ImportCfg)
                .padding([4, 8])
                .into(),
        );
        toolbar_items.push(
            button(text("Import .json").size(fs.small))
                .on_press(ProfileManagerMessage::ImportJson)
                .padding([4, 8])
                .into(),
        );
        toolbar_items.push(
            button(text("Snapshot Device").size(fs.small))
                .on_press_maybe(is_connected.then_some(ProfileManagerMessage::SnapshotFromDevice))
                .padding([4, 8])
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

                    // Thumbnail if screenshot exists
                    if let Some(ref ss_path) = entry.screenshot_path {
                        let handle = iced::widget::image::Handle::from_path(ss_path);
                        row_items.push(
                            iced::widget::image(handle)
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
                                is_connected
                                    .then_some(ProfileManagerMessage::ApplyProfileFromList(i))
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

        column![toolbar, rule::horizontal(1), profile_list, history_panel]
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
            button(text("Apply").size(fs.small))
                .on_press_maybe(is_connected.then_some(ProfileManagerMessage::ApplyProfile))
                .padding([4, 8])
                .style(button::success),
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
                "Capture current C64 screen as profile thumbnail",
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
                    text_input("category", &self.save_category)
                        .on_input(ProfileManagerMessage::EditCategory)
                        .size(fs.small as f32)
                        .width(Length::Fill),
                    pick_list(
                        vec!["Full".to_string(), "Overlay".to_string()],
                        Some(profile.profile_mode.to_string()),
                        ProfileManagerMessage::SetProfileMode,
                    )
                    .text_size(fs.small)
                    .width(Length::Fixed(80.0)),
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

        // Baseline
        let baseline_info = if let Some(bl) = &self.baseline_config {
            let count: usize = bl.values().map(|v| v.len()).sum();
            format!("{} cat, {} settings", bl.len(), count)
        } else {
            "Not captured".to_string()
        };

        let baseline_section = container(
            column![
                text("BASELINE").size(fs.tiny),
                button(text("Snapshot Baseline").size(fs.small))
                    .on_press(ProfileManagerMessage::SnapshotBaseline)
                    .padding([3, 6])
                    .width(Length::Fill),
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
            .spacing(4),
        )
        .padding(8);

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
                // Cartridge
                text("Cartridge").size(fs.tiny),
                row![
                    text_input("cartridge path...", cart_path)
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
                        .unwrap_or("Select a category")
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

        let items_view: Element<'_, ProfileManagerMessage> = if let Some(cat_name) =
            &self.selected_config_category
        {
            if let Some(items) = profile.config.get(cat_name) {
                let filter_lower = self.search_filter.to_lowercase();
                let mut sorted_keys: Vec<_> = items.keys().collect();
                sorted_keys.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

                let filtered: Vec<_> = sorted_keys
                    .into_iter()
                    .filter(|k| filter_lower.is_empty() || k.to_lowercase().contains(&filter_lower))
                    .collect();

                let diff_items = self.get_diff_for_category(cat_name);

                let item_views: Vec<Element<'_, ProfileManagerMessage>> = filtered
                    .iter()
                    .map(|key| {
                        let value = items.get(*key).unwrap();
                        let is_diff = diff_items
                            .as_ref()
                            .map(|d| d.contains_key(*key))
                            .unwrap_or(false);
                        self.view_config_item(cat_name, key, value, is_diff, fs)
                    })
                    .collect();

                scrollable(
                    Column::with_children(item_views)
                        .spacing(8)
                        .padding(iced::Padding::new(10.0).right(15.0)),
                )
                .height(Length::Fill)
                .into()
            } else {
                container(text("Category is empty").size(fs.normal))
                    .padding(20)
                    .into()
            }
        } else {
            container(text("Select a config category from the left panel").size(fs.normal))
                .padding(20)
                .center_x(Length::Fill)
                .into()
        };

        container(
            column![header, rule::horizontal(1), items_view]
                .spacing(0)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .into()
    }

    /// Render a single config key/value editor — reuses the same widget patterns
    /// as config_editor.rs view_option.
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

        let name_color = if is_diff && self.show_diff {
            iced::Color::from_rgb(0.9, 0.7, 0.0)
        } else {
            iced::Color::WHITE
        };

        let name_row =
            row![text(key).size(fs.normal).color(name_color),].align_y(iced::Alignment::Center);

        // Determine the best widget for this value
        let control: Element<'_, ProfileManagerMessage> = match value {
            serde_json::Value::Number(n) => {
                let current = n.as_i64().unwrap_or(0);
                let cat2 = cat.clone();
                let k2 = k.clone();
                row![text_input("", &current.to_string())
                    .on_input(move |v| {
                        let parsed = v.parse::<i64>().unwrap_or(current);
                        ProfileManagerMessage::ConfigValueChanged(
                            cat2.clone(),
                            k2.clone(),
                            serde_json::json!(parsed),
                        )
                    })
                    .size(fs.normal as f32)
                    .width(Length::Fixed(100.0)),]
                .into()
            }
            serde_json::Value::Bool(b) => {
                let cat2 = cat.clone();
                let k2 = k.clone();
                row![
                    toggler(*b)
                        .on_toggle(move |v| {
                            ProfileManagerMessage::ConfigValueChanged(
                                cat2.clone(),
                                k2.clone(),
                                serde_json::Value::Bool(v),
                            )
                        })
                        .size(fs.header),
                    text(if *b { "Yes" } else { "No" }).size(fs.normal),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center)
                .into()
            }
            _ => {
                // String or anything else — borrow from value when possible
                let current_str: &str = match value {
                    serde_json::Value::String(s) => s.as_str(),
                    serde_json::Value::Null => "",
                    _ => "",
                };
                // For non-string JSON values, format as string
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

                // Check for boolean-like strings
                let lower = display_str.to_lowercase();
                if matches!(
                    lower.as_str(),
                    "enabled" | "disabled" | "yes" | "no" | "on" | "off"
                ) {
                    let is_on = matches!(lower.as_str(), "enabled" | "yes" | "on");
                    let cat2 = cat.clone();
                    let k2 = k.clone();
                    row![
                        toggler(is_on)
                            .on_toggle(move |v| {
                                let str_val = if v { "Yes" } else { "No" };
                                ProfileManagerMessage::ConfigValueChanged(
                                    cat2.clone(),
                                    k2.clone(),
                                    serde_json::Value::String(str_val.to_string()),
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
        };

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

        container(column![name_row, control, diff_hint].spacing(3))
            .padding([8, 10])
            .width(Length::Fill)
            .style(container::bordered_box)
            .into()
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
                .padding([4, 8]),
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
