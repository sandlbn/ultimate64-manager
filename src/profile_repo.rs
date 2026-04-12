//! Git-backed profile repository.
//!
//! Manages a local git repository for storing, versioning, and sharing
//! Ultimate64 device profiles. Each profile lives in its own directory.
//!
//! Repository layout:
//!   profiles/
//!     games/
//!       last-ninja/
//!         profile.json
//!         original.cfg       (optional, preserved from import)
//!     demos/
//!       edge-of-disgrace/
//!         profile.json
//!   baselines/
//!     default.json

use crate::device_profile::DeviceProfile;
use std::path::{Path, PathBuf};

/// Summary info for a profile in the repository (used for listing).
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    /// Profile ID (directory name)
    pub id: String,
    /// Profile name
    pub name: String,
    /// Category folder (e.g. "games", "demos", "uncategorized")
    pub category: String,
    /// Profile mode (full/overlay)
    pub mode: String,
    /// Number of config categories
    pub config_categories: usize,
    /// Total settings count
    pub setting_count: usize,
    /// Tags
    pub tags: Vec<String>,
    /// Path to profile.json
    pub path: PathBuf,
    /// Absolute path to screenshot PNG (if exists)
    pub screenshot_path: Option<PathBuf>,
}

/// Stored baseline data: config values + optional schema (valid values, ranges).
pub struct StoredBaseline {
    pub config: crate::device_profile::ConfigTree,
    pub schema: Option<crate::device_profile::ConfigSchema>,
}

/// Manages a profile repository with optional git versioning.
/// If git is not installed, profiles still work — you just lose
/// version history, diffing, and push/pull.
pub struct ProfileRepo {
    /// Root path of the repository
    root: PathBuf,
    /// Whether the repo directory structure has been initialized
    initialized: bool,
    /// Whether the `git` binary is available on this system
    git_available: bool,
}

impl ProfileRepo {
    /// Create a new ProfileRepo at the given root path.
    pub fn new(root: PathBuf) -> Self {
        // The repo is "initialized" if the directory structure exists,
        // regardless of whether git was used.
        let initialized = root.join(".git").is_dir() || root.join("profiles").is_dir();
        let git_available = Self::check_git_available();
        Self {
            root,
            initialized,
            git_available,
        }
    }

    /// Check if git is installed and usable.
    fn check_git_available() -> bool {
        std::process::Command::new("git")
            .args(["--version"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Get the default repository path inside the app config directory.
    pub fn default_path() -> Option<PathBuf> {
        Some(
            dirs::config_dir()?
                .join("ultimate64-manager")
                .join("profiles-repo"),
        )
    }

    /// Root path of the repository.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the repo is initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Initialize the profile repository.
    /// Creates the directory structure. If git is available, also initializes
    /// a git repo for versioning. Works fine without git — profiles are just
    /// stored as plain files.
    pub fn init(&mut self) -> Result<(), String> {
        if self.initialized {
            return Ok(());
        }

        // Create directory structure (always — this is what makes profiles work)
        std::fs::create_dir_all(self.root.join("profiles"))
            .map_err(|e| format!("Failed to create profiles dir: {}", e))?;
        std::fs::create_dir_all(self.root.join("baselines"))
            .map_err(|e| format!("Failed to create baselines dir: {}", e))?;

        // Initialize git repo if git is available
        if self.git_available {
            let output = std::process::Command::new("git")
                .args(["init"])
                .current_dir(&self.root)
                .output()
                .map_err(|e| format!("Failed to run git init: {}", e))?;

            if output.status.success() {
                let gitignore = "# OS files\n.DS_Store\nThumbs.db\n\n# Temp files\n*.tmp\n*.bak\n";
                std::fs::write(self.root.join(".gitignore"), gitignore).ok();
                self.git_add(".");
                self.git_commit("Initialize profile repository");
            } else {
                log::warn!(
                    "git init failed (profiles will work without versioning): {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        } else {
            log::info!(
                "git not found — profiles will work without version history. \
                 Install git for versioning, diffs, and remote sync."
            );
        }

        self.initialized = true;
        log::info!("Profile repository initialized at {}", self.root.display());
        Ok(())
    }

    /// List all profiles in the repository.
    pub fn list_profiles(&self) -> Result<Vec<ProfileEntry>, String> {
        let profiles_dir = self.root.join("profiles");
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::new();
        self.scan_profiles_dir(&profiles_dir, "", &mut entries)?;
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(entries)
    }

    /// Recursively scan for profile.json files.
    fn scan_profiles_dir(
        &self,
        dir: &Path,
        category: &str,
        entries: &mut Vec<ProfileEntry>,
    ) -> Result<(), String> {
        let read_dir = std::fs::read_dir(dir)
            .map_err(|e| format!("Failed to read {}: {}", dir.display(), e))?;

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let profile_json = path.join("profile.json");
                if profile_json.exists() {
                    // This directory contains a profile
                    match self.load_profile_entry(&profile_json, category) {
                        Ok(entry) => entries.push(entry),
                        Err(e) => {
                            log::warn!(
                                "Failed to load profile at {}: {}",
                                profile_json.display(),
                                e
                            );
                        }
                    }
                } else {
                    // Subdirectory — treat as a category folder
                    let sub_category = if category.is_empty() {
                        entry.file_name().to_string_lossy().to_string()
                    } else {
                        format!("{}/{}", category, entry.file_name().to_string_lossy())
                    };
                    self.scan_profiles_dir(&path, &sub_category, entries)?;
                }
            }
        }
        Ok(())
    }

    /// Load summary info from a profile.json file.
    fn load_profile_entry(&self, path: &Path, category: &str) -> Result<ProfileEntry, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        let profile: DeviceProfile =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse profile: {}", e))?;

        // Resolve screenshot path if metadata.screenshot is set
        let screenshot_path = if !profile.metadata.screenshot.is_empty() {
            let profile_dir = path.parent().unwrap_or(Path::new("."));
            let sp = profile_dir.join(&profile.metadata.screenshot);
            if sp.exists() {
                Some(sp)
            } else {
                None
            }
        } else {
            // Also check for screenshot.png in profile dir by convention
            let profile_dir = path.parent().unwrap_or(Path::new("."));
            let sp = profile_dir.join("screenshot.png");
            if sp.exists() {
                Some(sp)
            } else {
                None
            }
        };

        Ok(ProfileEntry {
            id: profile.id.clone(),
            name: profile.name.clone(),
            category: if category.is_empty() {
                "uncategorized".to_string()
            } else {
                category.to_string()
            },
            mode: profile.profile_mode.to_string(),
            config_categories: profile.config.len(),
            setting_count: profile.setting_count(),
            tags: profile.tags.clone(),
            path: path.to_path_buf(),
            screenshot_path,
        })
    }

    /// Save a profile to the repository.
    /// Creates the directory structure: profiles/<category>/<id>/profile.json
    pub fn save_profile(
        &mut self,
        profile: &DeviceProfile,
        category: &str,
        original_cfg: Option<&str>,
    ) -> Result<PathBuf, String> {
        self.ensure_initialized()?;

        let cat = if category.is_empty() {
            "uncategorized"
        } else {
            category
        };

        let profile_dir = self.root.join("profiles").join(cat).join(&profile.id);
        std::fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create profile dir: {}", e))?;

        let profile_path = profile_dir.join("profile.json");
        let json = serde_json::to_string_pretty(profile)
            .map_err(|e| format!("Failed to serialize profile: {}", e))?;
        std::fs::write(&profile_path, &json)
            .map_err(|e| format!("Failed to write profile: {}", e))?;

        // Optionally store original .cfg
        if let Some(cfg_content) = original_cfg {
            let cfg_path = profile_dir.join("original.cfg");
            std::fs::write(&cfg_path, cfg_content)
                .map_err(|e| format!("Failed to write original.cfg: {}", e))?;
        }

        log::info!(
            "Saved profile '{}' to {}",
            profile.name,
            profile_path.display()
        );
        Ok(profile_path)
    }

    /// Load a profile from a path.
    pub fn load_profile(&self, path: &Path) -> Result<DeviceProfile, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse profile: {}", e))
    }

    /// Delete a profile from the repository.
    pub fn delete_profile(&mut self, profile_path: &Path) -> Result<(), String> {
        let profile_dir = profile_path.parent().ok_or("Invalid profile path")?;

        if profile_dir.exists() {
            std::fs::remove_dir_all(profile_dir)
                .map_err(|e| format!("Failed to delete profile: {}", e))?;
        }
        Ok(())
    }

    /// Save a baseline configuration with optional schema (valid values, ranges).
    pub fn save_baseline(
        &mut self,
        name: &str,
        config: &crate::device_profile::ConfigTree,
        schema: Option<&crate::device_profile::ConfigSchema>,
    ) -> Result<PathBuf, String> {
        self.ensure_initialized()?;

        let baselines_dir = self.root.join("baselines");
        std::fs::create_dir_all(&baselines_dir)
            .map_err(|e| format!("Failed to create baselines dir: {}", e))?;

        let filename = format!("{}.json", crate::device_profile::slugify(name));
        let path = baselines_dir.join(&filename);

        #[derive(serde::Serialize)]
        struct BaselineFile<'a> {
            name: String,
            config: &'a crate::device_profile::ConfigTree,
            #[serde(skip_serializing_if = "Option::is_none")]
            schema: Option<&'a crate::device_profile::ConfigSchema>,
            saved_at: String,
        }

        let baseline = BaselineFile {
            name: name.to_string(),
            config,
            schema,
            saved_at: chrono::Utc::now().to_rfc3339(),
        };

        let json = serde_json::to_string_pretty(&baseline)
            .map_err(|e| format!("Failed to serialize baseline: {}", e))?;
        std::fs::write(&path, &json).map_err(|e| format!("Failed to write baseline: {}", e))?;

        log::info!("Saved baseline '{}' to {}", name, path.display());
        Ok(path)
    }

    /// Load a baseline configuration + schema.
    pub fn load_baseline(&self, name: &str) -> Result<StoredBaseline, String> {
        let filename = format!("{}.json", crate::device_profile::slugify(name));
        let path = self.root.join("baselines").join(&filename);

        let json = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read baseline: {}", e))?;

        #[derive(serde::Deserialize)]
        struct BaselineFile {
            config: crate::device_profile::ConfigTree,
            #[serde(default)]
            schema: Option<crate::device_profile::ConfigSchema>,
        }

        let baseline: BaselineFile =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse baseline: {}", e))?;
        Ok(StoredBaseline {
            config: baseline.config,
            schema: baseline.schema,
        })
    }

    /// List available baselines.
    pub fn list_baselines(&self) -> Result<Vec<String>, String> {
        let dir = self.root.join("baselines");
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| format!("Failed to read baselines dir: {}", e))?
            .flatten()
        {
            if let Some(name) = entry.path().file_stem() {
                names.push(name.to_string_lossy().to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    // === Git operations ===

    /// Commit current changes to the repository. No-op without git.
    pub fn commit(&mut self, message: &str) -> Result<(), String> {
        if !self.git_available {
            return Ok(());
        }
        self.ensure_initialized()?;
        self.git_add(".")?;

        // Check if there's anything to commit
        let status = self.git_status()?;
        if status.is_empty() || status.trim() == "" {
            log::info!("Nothing to commit");
            return Ok(());
        }

        self.git_commit(message)
    }

    /// Get git status. Returns empty string without git.
    pub fn git_status(&self) -> Result<String, String> {
        if !self.git_available {
            return Ok(String::new());
        }
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git status: {}", e))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get git log (last N commits). Returns empty without git.
    pub fn git_log(&self, count: usize) -> Result<Vec<String>, String> {
        if !self.git_available {
            return Ok(vec!["(git not installed — no history)".to_string()]);
        }
        let output = std::process::Command::new("git")
            .args(["log", &format!("-{}", count), "--oneline", "--no-decorate"])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git log: {}", e))?;

        let log_text = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(log_text.lines().map(|l| l.to_string()).collect())
    }

    /// Pull from remote (if configured). No-op without git.
    pub fn pull(&self) -> Result<String, String> {
        if !self.git_available {
            return Err("git is not installed".to_string());
        }
        let output = std::process::Command::new("git")
            .args(["pull", "--rebase"])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git pull: {}", e))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(format!(
                "git pull failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    /// Push to remote (if configured). No-op without git.
    pub fn push(&self) -> Result<String, String> {
        if !self.git_available {
            return Err("git is not installed".to_string());
        }
        let output = std::process::Command::new("git")
            .args(["push"])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git push: {}", e))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(format!(
                "git push failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    /// Check if a remote is configured. False without git.
    pub fn has_remote(&self) -> bool {
        if !self.initialized || !self.git_available {
            return false;
        }
        std::process::Command::new("git")
            .args(["remote"])
            .current_dir(&self.root)
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    // === Internal helpers ===

    fn ensure_initialized(&mut self) -> Result<(), String> {
        if !self.initialized {
            self.init()?;
        }
        Ok(())
    }

    /// Run `git add`. No-op if git is not available.
    fn git_add(&self, path: &str) -> Result<(), String> {
        if !self.git_available {
            return Ok(());
        }
        let output = std::process::Command::new("git")
            .args(["add", path])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git add: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git add failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    /// Run `git commit`. No-op if git is not available.
    fn git_commit(&self, message: &str) -> Result<(), String> {
        if !self.git_available {
            return Ok(());
        }
        let output = std::process::Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(&self.root)
            .output()
            .map_err(|e| format!("Failed to run git commit: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            // "nothing to commit" is not an error
            if stderr.contains("nothing to commit") {
                Ok(())
            } else {
                Err(format!("git commit failed: {}", stderr))
            }
        }
    }
}

/// Async wrappers for profile repo operations.

pub async fn list_profiles_async(root: PathBuf) -> Result<Vec<ProfileEntry>, String> {
    tokio::task::spawn_blocking(move || {
        let repo = ProfileRepo::new(root);
        repo.list_profiles()
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

pub async fn save_profile_async(
    root: PathBuf,
    profile: DeviceProfile,
    category: String,
    original_cfg: Option<String>,
    commit_message: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let mut repo = ProfileRepo::new(root);
        repo.save_profile(&profile, &category, original_cfg.as_deref())?;

        let msg = commit_message.unwrap_or_else(|| {
            format!(
                "{} profile: {}",
                if profile.source_format == crate::device_profile::SourceFormat::Cfg {
                    "Import cfg"
                } else {
                    "Save"
                },
                profile.name
            )
        });
        repo.commit(&msg)?;

        Ok(format!(
            "Saved profile '{}' ({} categories, {} settings)",
            profile.name,
            profile.config.len(),
            profile.setting_count()
        ))
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

pub async fn load_profile_async(path: PathBuf) -> Result<DeviceProfile, String> {
    tokio::task::spawn_blocking(move || {
        let repo = ProfileRepo::new(path.parent().unwrap_or(Path::new(".")).to_path_buf());
        repo.load_profile(&path)
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

pub async fn delete_profile_async(
    root: PathBuf,
    profile_path: PathBuf,
    profile_name: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let mut repo = ProfileRepo::new(root);
        repo.delete_profile(&profile_path)?;
        repo.commit(&format!("Delete profile: {}", profile_name))?;
        Ok(format!("Deleted profile: {}", profile_name))
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

pub async fn init_repo_async(root: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let mut repo = ProfileRepo::new(root);
        repo.init()?;
        Ok("Profile repository initialized".to_string())
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}
