use crate::settings::AppSettings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A named configuration profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub settings: AppSettings,
}

impl Profile {
    pub fn new(name: String) -> Self {
        Self {
            name,
            settings: AppSettings::default(),
        }
    }
}

/// Manages multiple configuration profiles
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileManager {
    pub profiles: Vec<Profile>,
    pub active_profile: String,
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self {
            profiles: vec![Profile::new("Default".to_string())],
            active_profile: "Default".to_string(),
        }
    }
}

impl ProfileManager {
    fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-manager");
        fs::create_dir_all(&config_dir)?;
        Ok(config_dir.join("profiles.json"))
    }

    pub fn load() -> Self {
        match Self::try_load() {
            Ok(manager) => manager,
            Err(e) => {
                log::warn!("Could not load profiles: {}. Using defaults.", e);
                // Try to migrate from old settings.json
                Self::migrate_from_legacy()
            }
        }
    }

    fn try_load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_file = Self::config_path()?;
        if config_file.exists() {
            let contents = fs::read_to_string(&config_file)?;
            let manager: ProfileManager = serde_json::from_str(&contents)?;
            log::info!("Loaded {} profiles", manager.profiles.len());
            Ok(manager)
        } else {
            Err("No profiles file found".into())
        }
    }

    /// Migrate from legacy single settings.json to profiles
    fn migrate_from_legacy() -> Self {
        if let Ok(settings) = AppSettings::load() {
            log::info!("Migrating legacy settings to Default profile");
            let mut manager = Self::default();
            if let Some(profile) = manager.profiles.first_mut() {
                profile.settings = settings;
            }
            let _ = manager.save();
            manager
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_file = Self::config_path()?;
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&config_file, contents)?;
        log::info!("Profiles saved successfully");
        Ok(())
    }

    pub fn active_settings(&self) -> &AppSettings {
        self.profiles
            .iter()
            .find(|p| p.name == self.active_profile)
            .map(|p| &p.settings)
            .unwrap_or_else(|| &self.profiles[0].settings)
    }

    pub fn active_settings_mut(&mut self) -> &mut AppSettings {
        let name = self.active_profile.clone();

        // Find the index first to avoid borrow issues
        let index = self
            .profiles
            .iter()
            .position(|p| p.name == name)
            .unwrap_or(0);

        &mut self.profiles[index].settings
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }

    pub fn add_profile(&mut self, name: String) -> bool {
        if self.profiles.iter().any(|p| p.name == name) {
            return false;
        }
        self.profiles.push(Profile::new(name));
        true
    }

    pub fn duplicate_profile(&mut self, source_name: &str, new_name: String) -> bool {
        if self.profiles.iter().any(|p| p.name == new_name) {
            return false;
        }
        if let Some(source) = self.profiles.iter().find(|p| p.name == source_name) {
            let mut new_profile = source.clone();
            new_profile.name = new_name;
            self.profiles.push(new_profile);
            true
        } else {
            false
        }
    }

    pub fn delete_profile(&mut self, name: &str) -> bool {
        if self.profiles.len() <= 1 || name == self.active_profile {
            return false;
        }
        self.profiles.retain(|p| p.name != name);
        true
    }

    pub fn rename_profile(&mut self, old_name: &str, new_name: String) -> bool {
        if self.profiles.iter().any(|p| p.name == new_name) {
            return false;
        }
        if let Some(profile) = self.profiles.iter_mut().find(|p| p.name == old_name) {
            if self.active_profile == old_name {
                self.active_profile = new_name.clone();
            }
            profile.name = new_name;
            true
        } else {
            false
        }
    }

    pub fn switch_profile(&mut self, name: &str) -> bool {
        if self.profiles.iter().any(|p| p.name == name) {
            self.active_profile = name.to_string();
            true
        } else {
            false
        }
    }
}
