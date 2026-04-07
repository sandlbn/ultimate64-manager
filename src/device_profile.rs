//! Canonical device profile data model.
//!
//! A device profile represents a full or partial Ultimate64/Elite-II configuration
//! stored as structured JSON. It separates machine config from mount instructions
//! and launch behavior.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The canonical profile format — source of truth for all profile operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    /// Unique identifier (slug-style, e.g. "last-ninja")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Optional description
    #[serde(default)]
    pub description: String,
    /// Whether this is a full snapshot or a partial overlay
    #[serde(default)]
    pub profile_mode: ProfileMode,
    /// What format was originally imported
    #[serde(default)]
    pub source_format: SourceFormat,
    /// Which device families this profile targets
    #[serde(default)]
    pub device_models: Vec<String>,
    /// Free-form tags for organization
    #[serde(default)]
    pub tags: Vec<String>,
    /// The actual configuration: category -> key -> value
    #[serde(default)]
    pub config: ConfigTree,
    /// Media mount mappings
    #[serde(default)]
    pub mounts: MountMap,
    /// Launch behavior
    #[serde(default)]
    pub launch: LaunchSettings,
    /// Additional metadata
    #[serde(default)]
    pub metadata: ProfileMetadata,
}

/// Configuration tree: category name -> (key name -> value)
pub type ConfigTree = HashMap<String, HashMap<String, serde_json::Value>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProfileMode {
    #[default]
    Full,
    Overlay,
}

impl std::fmt::Display for ProfileMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileMode::Full => write!(f, "Full"),
            ProfileMode::Overlay => write!(f, "Overlay"),
        }
    }
}

impl ProfileMode {
    pub const ALL: [ProfileMode; 2] = [ProfileMode::Full, ProfileMode::Overlay];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SourceFormat {
    #[default]
    Json,
    Cfg,
    Api,
}

impl std::fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceFormat::Json => write!(f, "JSON"),
            SourceFormat::Cfg => write!(f, "CFG"),
            SourceFormat::Api => write!(f, "API"),
        }
    }
}

/// Media mount mappings for drives, cartridge, and tape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_a: Option<MountEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_b: Option<MountEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cartridge: Option<MountEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape: Option<MountEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountEntry {
    /// Media type: "disk", "cartridge", "tape"
    #[serde(rename = "type")]
    pub media_type: String,
    /// Path to media file (relative to profile repo, or absolute on device)
    pub path: String,
    /// Whether to auto-load after mounting
    #[serde(default)]
    pub autoload: bool,
}

/// Launch behavior when applying a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSettings {
    /// Restore baseline config before applying overlay
    #[serde(default)]
    pub restore_baseline_first: bool,
    /// Reset machine after applying config + mounts
    #[serde(default)]
    pub reset_after_apply: bool,
}

impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            restore_baseline_first: false,
            reset_after_apply: false,
        }
    }
}

/// Additional profile metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileMetadata {
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub firmware_hint: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub origin_device_model: String,
    #[serde(default)]
    pub origin_firmware_version: String,
    /// Path to screenshot PNG (relative to profile dir)
    #[serde(default)]
    pub screenshot: String,
}

impl DeviceProfile {
    /// Create a new empty profile with the given id and name.
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            profile_mode: ProfileMode::Full,
            source_format: SourceFormat::Json,
            device_models: vec!["ultimate64".to_string()],
            tags: Vec::new(),
            config: HashMap::new(),
            mounts: MountMap::default(),
            launch: LaunchSettings::default(),
            metadata: ProfileMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
        }
    }

    /// Create a profile from a config tree (e.g. from API read or preset import).
    pub fn from_config(id: &str, name: &str, config: ConfigTree, mode: ProfileMode) -> Self {
        let mut profile = Self::new(id, name);
        profile.config = config;
        profile.profile_mode = mode;
        profile
    }

    /// Convert from an existing ConfigPreset (backward compatibility).
    pub fn from_preset(preset: &crate::config_presets::ConfigPreset) -> Self {
        let name = preset
            .name
            .clone()
            .unwrap_or_else(|| "Imported Preset".to_string());
        let id = slugify(&name);
        let mut profile = Self::new(&id, &name);
        profile.description = preset.description.clone().unwrap_or_default();
        profile.config = preset.settings.clone();
        profile.source_format = SourceFormat::Json;
        // Detect if this looks like a full backup
        if preset.settings.len() >= 10 {
            profile.profile_mode = ProfileMode::Full;
        } else {
            profile.profile_mode = ProfileMode::Overlay;
        }
        profile
    }

    /// Convert to a ConfigPreset for backward compatibility with existing apply logic.
    pub fn to_preset(&self) -> crate::config_presets::ConfigPreset {
        crate::config_presets::ConfigPreset {
            name: Some(self.name.clone()),
            description: Some(self.description.clone()),
            settings: self.config.clone(),
        }
    }

    /// Count total settings across all categories.
    pub fn setting_count(&self) -> usize {
        self.config.values().map(|v| v.len()).sum()
    }

    /// Get sorted list of categories.
    pub fn categories(&self) -> Vec<&String> {
        let mut cats: Vec<_> = self.config.keys().collect();
        cats.sort();
        cats
    }

    /// Deep-merge an overlay on top of this profile's config.
    /// Returns a new merged config tree.
    pub fn merge_overlay(&self, overlay: &ConfigTree) -> ConfigTree {
        deep_merge(&self.config, overlay)
    }
}

/// Deep merge: overlay values override base values at the category/key level.
pub fn deep_merge(base: &ConfigTree, overlay: &ConfigTree) -> ConfigTree {
    let mut result = base.clone();
    for (category, items) in overlay {
        let entry = result.entry(category.clone()).or_default();
        for (key, value) in items {
            entry.insert(key.clone(), value.clone());
        }
    }
    result
}

/// Compute a diff: only keys in `modified` that differ from `base`.
/// Returns an overlay containing only changed settings.
pub fn diff_configs(base: &ConfigTree, modified: &ConfigTree) -> ConfigTree {
    let mut diff = ConfigTree::new();

    // Check all categories in modified
    for (category, mod_items) in modified {
        let base_items = base.get(category);
        for (key, mod_value) in mod_items {
            let differs = match base_items.and_then(|b| b.get(key)) {
                Some(base_value) => !values_semantically_equal(base_value, mod_value),
                None => true, // New key not in base
            };
            if differs {
                diff.entry(category.clone())
                    .or_default()
                    .insert(key.clone(), mod_value.clone());
            }
        }
    }

    diff
}

/// Compare two JSON values semantically, handling type mismatches gracefully.
///
/// The Ultimate64 API may represent the same value as a number or string
/// depending on context (e.g. `Number(1)` vs `String(" 1")`). A naive
/// `==` comparison would see these as different, causing false-positive diffs.
///
/// Strategy: normalize both values to their trimmed string form and compare.
fn values_semantically_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    // Fast path: identical JSON values
    if a == b {
        return true;
    }

    // Normalize to comparable strings and check trimmed equality
    let a_str = value_to_comparable_string(a);
    let b_str = value_to_comparable_string(b);
    a_str == b_str
}

fn value_to_comparable_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::Null => String::new(),
        _ => v.to_string(),
    }
}

/// Convert a name to a URL-safe slug.
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Import a JSON backup (with "settings" key) into a DeviceProfile.
pub fn import_json_backup(json_str: &str) -> Result<DeviceProfile, String> {
    // Try parsing as a DeviceProfile first
    if let Ok(profile) = serde_json::from_str::<DeviceProfile>(json_str) {
        if !profile.config.is_empty() {
            return Ok(profile);
        }
    }

    // Try parsing as a ConfigPreset-style backup (has "settings" key)
    #[derive(Deserialize)]
    struct BackupFormat {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        settings: Option<ConfigTree>,
    }

    let backup: BackupFormat =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {}", e))?;

    if let Some(settings) = backup.settings {
        let name = backup.name.unwrap_or_else(|| "Imported Backup".to_string());
        let id = slugify(&name);
        let mut profile = DeviceProfile::new(&id, &name);
        profile.description = backup.description.unwrap_or_default();
        profile.config = settings;
        profile.source_format = SourceFormat::Json;
        if profile.config.len() >= 10 {
            profile.profile_mode = ProfileMode::Full;
        } else {
            profile.profile_mode = ProfileMode::Overlay;
        }
        profile.metadata.notes = "Imported from JSON backup".to_string();
        Ok(profile)
    } else {
        Err("JSON does not contain 'settings' or 'config' field".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Last Ninja"), "last-ninja");
        assert_eq!(slugify("Edge of Disgrace!"), "edge-of-disgrace");
        assert_eq!(slugify("  Hello  World  "), "hello-world");
    }

    #[test]
    fn test_deep_merge() {
        let mut base = ConfigTree::new();
        let mut cat = HashMap::new();
        cat.insert("Drive".to_string(), serde_json::json!("Disabled"));
        cat.insert("Drive Type".to_string(), serde_json::json!("1541"));
        base.insert("Drive A Settings".to_string(), cat);

        let mut overlay = ConfigTree::new();
        let mut cat2 = HashMap::new();
        cat2.insert("Drive".to_string(), serde_json::json!("Enabled"));
        overlay.insert("Drive A Settings".to_string(), cat2);

        let merged = deep_merge(&base, &overlay);
        let drive_a = merged.get("Drive A Settings").unwrap();
        assert_eq!(drive_a.get("Drive").unwrap(), &serde_json::json!("Enabled"));
        assert_eq!(
            drive_a.get("Drive Type").unwrap(),
            &serde_json::json!("1541")
        );
    }

    #[test]
    fn test_diff_configs() {
        let mut base = ConfigTree::new();
        let mut cat = HashMap::new();
        cat.insert("Drive".to_string(), serde_json::json!("Disabled"));
        cat.insert("Drive Type".to_string(), serde_json::json!("1541"));
        base.insert("Drive A Settings".to_string(), cat);

        let mut modified = ConfigTree::new();
        let mut cat2 = HashMap::new();
        cat2.insert("Drive".to_string(), serde_json::json!("Enabled"));
        cat2.insert("Drive Type".to_string(), serde_json::json!("1541"));
        modified.insert("Drive A Settings".to_string(), cat2);

        let diff = diff_configs(&base, &modified);
        let drive_a = diff.get("Drive A Settings").unwrap();
        assert_eq!(drive_a.len(), 1);
        assert_eq!(drive_a.get("Drive").unwrap(), &serde_json::json!("Enabled"));
    }

    #[test]
    fn test_diff_type_mismatch_no_false_positive() {
        // Old baseline stored CPU Speed as Number(1), new profile has String(" 1")
        // These are semantically the same — diff should be empty
        let mut base = ConfigTree::new();
        let mut cat = HashMap::new();
        cat.insert("CPU Speed".to_string(), serde_json::json!(1));
        cat.insert("Drive Bus ID".to_string(), serde_json::json!(8));
        base.insert("U64 Specific Settings".to_string(), cat);

        let mut modified = ConfigTree::new();
        let mut cat2 = HashMap::new();
        cat2.insert("CPU Speed".to_string(), serde_json::json!(" 1"));
        cat2.insert("Drive Bus ID".to_string(), serde_json::json!(8));
        modified.insert("U64 Specific Settings".to_string(), cat2);

        let diff = diff_configs(&base, &modified);
        assert!(
            diff.is_empty(),
            "Expected empty diff for Number(1) vs String(\" 1\"), got: {:?}",
            diff
        );
    }

    #[test]
    fn test_diff_detects_real_change_across_types() {
        // Number(1) vs String(" 2") — these are truly different
        let mut base = ConfigTree::new();
        let mut cat = HashMap::new();
        cat.insert("CPU Speed".to_string(), serde_json::json!(1));
        base.insert("U64".to_string(), cat);

        let mut modified = ConfigTree::new();
        let mut cat2 = HashMap::new();
        cat2.insert("CPU Speed".to_string(), serde_json::json!(" 2"));
        modified.insert("U64".to_string(), cat2);

        let diff = diff_configs(&base, &modified);
        assert_eq!(diff.get("U64").unwrap().len(), 1);
    }
}
