//! Configuration presets module
//!
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A configuration preset containing settings for one or more categories
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigPreset {
    /// Optional name/description for the preset
    #[serde(default)]
    pub name: Option<String>,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
    /// The actual configuration data: category -> (item_name -> value)
    pub settings: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl ConfigPreset {
    /// Create a new empty preset
    pub fn new() -> Self {
        Self {
            name: None,
            description: None,
            settings: HashMap::new(),
        }
    }

    /// Create a preset with a name
    pub fn with_name(name: &str) -> Self {
        Self {
            name: Some(name.to_string()),
            description: None,
            settings: HashMap::new(),
        }
    }

    /// Add a setting to the preset
    pub fn add_setting(&mut self, category: &str, item_name: &str, value: serde_json::Value) {
        self.settings
            .entry(category.to_string())
            .or_insert_with(HashMap::new)
            .insert(item_name.to_string(), value);
    }

    /// Get the total number of settings in the preset
    pub fn setting_count(&self) -> usize {
        self.settings.values().map(|v| v.len()).sum()
    }

    /// Get list of categories in the preset
    pub fn categories(&self) -> Vec<&String> {
        self.settings.keys().collect()
    }
}

impl Default for ConfigPreset {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the default presets directory inside the app config directory.
/// Used as the default starting directory for save/load file dialogs.
pub fn presets_dir() -> Option<PathBuf> {
    let dir = dirs::config_dir()?
        .join("ultimate64-manager")
        .join("presets");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Save a configuration preset to a JSON file
pub fn save_preset_to_file(preset: &ConfigPreset, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(preset)
        .map_err(|e| format!("Failed to serialize preset: {}", e))?;
    std::fs::write(path, json).map_err(|e| format!("Failed to write file: {}", e))?;
    log::info!(
        "Saved preset with {} settings to {}",
        preset.setting_count(),
        path.display()
    );
    Ok(())
}

/// Load a configuration preset from a JSON file
pub fn load_preset_from_file(path: &Path) -> Result<ConfigPreset, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let preset: ConfigPreset =
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse preset: {}", e))?;
    log::info!(
        "Loaded preset with {} settings from {}",
        preset.setting_count(),
        path.display()
    );
    Ok(preset)
}

/// Async wrapper for saving preset (for use with file dialog)
pub async fn save_preset_async(
    preset: ConfigPreset,
    path: std::path::PathBuf,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        save_preset_to_file(&preset, &path)?;
        Ok(format!(
            "Saved {} settings to {}",
            preset.setting_count(),
            path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
        ))
    })
    .await
    .map_err(|e| format!("Task error: {}", e))?
}

/// Async wrapper for loading preset (for use with file dialog)
pub async fn load_preset_async(path: std::path::PathBuf) -> Result<ConfigPreset, String> {
    tokio::task::spawn_blocking(move || load_preset_from_file(&path))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

/// Create a preset from current category items
pub fn create_preset_from_items(
    category: &str,
    items: &HashMap<String, serde_json::Value>,
    name: Option<&str>,
) -> ConfigPreset {
    let mut preset = ConfigPreset::new();
    preset.name = name.map(|s| s.to_string());
    for (item_name, value) in items {
        preset.add_setting(category, item_name, value.clone());
    }
    preset
}
