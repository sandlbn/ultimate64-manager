//! CFG format parser and exporter.
//!
//! Handles translation between Ultimate64 native .cfg files (INI-style)
//! and the canonical JSON config tree.
//!
//! Translation rules:
//! - Each [Section] becomes a top-level key in the config tree
//! - Each key=value line becomes a nested entry under its section
//! - Original spelling, spaces, capitalization are preserved exactly
//! - Empty values are preserved as empty strings
//! - Blank lines and comment lines are ignored
//! - Values are initially parsed as strings; known numeric keys are converted

use crate::device_profile::{ConfigTree, DeviceProfile, ProfileMode, SourceFormat};

// No hardcoded key lists — integer detection is done heuristically.
// See `normalize_value` for the logic.

/// Parse a .cfg file (INI-style) into a ConfigTree.
///
/// Preserves original key names exactly. Values after `=` are preserved
/// as-is (including leading/trailing spaces) because the Ultimate64 API
/// uses exact string matching for enum values like `" 0 dB"` and `" 1"`.
/// Only known integer keys have their values trimmed and parsed as numbers.
pub fn parse_cfg(input: &str) -> Result<ConfigTree, String> {
    let mut config = ConfigTree::new();
    let mut current_section: Option<String> = None;

    for (line_num, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();

        // Skip empty lines
        if line.is_empty() {
            continue;
        }

        // Skip comment lines
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header: [Section Name]
        if line.starts_with('[') && line.ends_with(']') {
            let section_name = line[1..line.len() - 1].to_string();
            if section_name.is_empty() {
                return Err(format!("Empty section name at line {}", line_num + 1));
            }
            current_section = Some(section_name);
            continue;
        }

        // Key=Value line
        if let Some(eq_pos) = raw_line.find('=') {
            let key = raw_line[..eq_pos].trim().to_string();
            let value_str = raw_line[eq_pos + 1..].to_string();

            if key.is_empty() {
                continue;
            }

            let section = current_section.as_ref().ok_or_else(|| {
                format!(
                    "Key '{}' at line {} is outside any section",
                    key,
                    line_num + 1
                )
            })?;

            let value = normalize_value(&key, &value_str);

            config
                .entry(section.clone())
                .or_default()
                .insert(key, value);
        }
        // Lines that don't match any pattern are silently skipped
    }

    Ok(config)
}

/// Normalize a value string into a serde_json::Value.
///
/// Values are preserved exactly as they appear after the `=` sign because the
/// Ultimate64 API uses exact string matching — values like `" 0 dB"` and `" 1"`
/// have significant leading spaces.
///
/// The only exception: values that are *purely* numeric with no surrounding
/// whitespace (e.g. `"8"`, `"0"`, `"-100"`) are stored as JSON numbers.
/// This avoids hardcoding a list of known integer keys that would break when
/// the firmware schema changes.
///
/// Values like `" 1"` (leading space) or `"3000"` after whitespace are kept as
/// strings because they may be enum values or have formatting significance.
fn normalize_value(_key: &str, raw_value: &str) -> serde_json::Value {
    // Only convert to number if the raw value (after =) is *exactly* a number
    // with no leading/trailing whitespace — this means the .cfg had e.g. `Key=8`
    // not `Key= 8` (which would be a string with a leading space).
    if !raw_value.is_empty() && raw_value == raw_value.trim() {
        if let Ok(n) = raw_value.parse::<i64>() {
            return serde_json::Value::Number(serde_json::Number::from(n));
        }
    }

    // Preserve the raw value as-is (leading/trailing spaces are significant)
    serde_json::Value::String(raw_value.to_string())
}

/// Parse a .cfg file and wrap it in a DeviceProfile.
pub fn import_cfg(input: &str, name: &str) -> Result<DeviceProfile, String> {
    let config = parse_cfg(input)?;
    let id = crate::device_profile::slugify(name);
    let mut profile = DeviceProfile::new(&id, name);
    profile.config = config;
    profile.source_format = SourceFormat::Cfg;
    // Heuristic: 10+ categories likely means a full config
    if profile.config.len() >= 10 {
        profile.profile_mode = ProfileMode::Full;
    } else {
        profile.profile_mode = ProfileMode::Overlay;
    }
    profile.metadata.notes = "Imported from .cfg file".to_string();
    Ok(profile)
}

/// Export a ConfigTree to .cfg format (INI-style).
///
/// Categories are emitted as [Section] headers, keys as key=value lines.
/// Sections are separated by blank lines.
pub fn export_cfg(config: &ConfigTree) -> String {
    let mut output = String::new();

    // Sort categories for deterministic output
    let mut categories: Vec<_> = config.keys().collect();
    categories.sort();

    for (i, category) in categories.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        output.push('[');
        output.push_str(category);
        output.push_str("]\n");

        if let Some(items) = config.get(*category) {
            // Sort keys for deterministic output
            let mut keys: Vec<_> = items.keys().collect();
            keys.sort();

            for key in keys {
                if let Some(value) = items.get(key) {
                    output.push_str(key);
                    output.push('=');
                    output.push_str(&value_to_cfg_string(value));
                    output.push('\n');
                }
            }
        }
    }

    output
}

/// Export a DeviceProfile's config to .cfg format.
pub fn export_profile_cfg(profile: &DeviceProfile) -> String {
    export_cfg(&profile.config)
}

/// Convert a JSON value to a string suitable for .cfg output.
fn value_to_cfg_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => {
            if *b {
                "Yes".to_string()
            } else {
                "No".to_string()
            }
        }
        serde_json::Value::Null => String::new(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CFG: &str = r#"[Audio Mixer]
Vol UltiSid 1=+1 dB
Vol UltiSid 2= 0 dB
Pan UltiSID 1=Center

[Drive A Settings]
Drive=Enabled
Drive Type=1541
Drive Bus ID=8
"#;

    #[test]
    fn test_parse_cfg() {
        let config = parse_cfg(SAMPLE_CFG).unwrap();
        assert_eq!(config.len(), 2);

        let audio = config.get("Audio Mixer").unwrap();
        // Values are preserved as-is including leading spaces (API needs exact match)
        assert_eq!(
            audio.get("Vol UltiSid 1").unwrap(),
            &serde_json::json!("+1 dB")
        );
        assert_eq!(
            audio.get("Vol UltiSid 2").unwrap(),
            &serde_json::json!(" 0 dB")
        );
        assert_eq!(
            audio.get("Pan UltiSID 1").unwrap(),
            &serde_json::json!("Center")
        );

        let drive = config.get("Drive A Settings").unwrap();
        assert_eq!(drive.get("Drive").unwrap(), &serde_json::json!("Enabled"));
        // Drive Bus ID is a known integer key — gets parsed to number
        assert_eq!(drive.get("Drive Bus ID").unwrap(), &serde_json::json!(8));
    }

    #[test]
    fn test_leading_space_preserved() {
        // The Ultimate64 API uses exact string matching — " 1" != "1"
        let cfg = "[U64 Specific Settings]\nCPU Speed= 1\n";
        let config = parse_cfg(cfg).unwrap();
        let u64_settings = config.get("U64 Specific Settings").unwrap();
        assert_eq!(
            u64_settings.get("CPU Speed").unwrap(),
            &serde_json::json!(" 1")
        );
    }

    #[test]
    fn test_roundtrip() {
        let config = parse_cfg(SAMPLE_CFG).unwrap();
        let exported = export_cfg(&config);
        let reparsed = parse_cfg(&exported).unwrap();

        // Semantic equality (values should match)
        for (category, items) in &config {
            let reparsed_items = reparsed.get(category).unwrap();
            for (key, value) in items {
                assert_eq!(reparsed_items.get(key).unwrap(), value);
            }
        }
    }

    #[test]
    fn test_empty_values() {
        let cfg = "[Test]\nKey1=\nKey2=value\n";
        let config = parse_cfg(cfg).unwrap();
        let test = config.get("Test").unwrap();
        assert_eq!(test.get("Key1").unwrap(), &serde_json::json!(""));
        assert_eq!(test.get("Key2").unwrap(), &serde_json::json!("value"));
    }

    #[test]
    fn test_import_cfg() {
        let profile = import_cfg(SAMPLE_CFG, "Test Config").unwrap();
        assert_eq!(profile.id, "test-config");
        assert_eq!(profile.name, "Test Config");
        assert_eq!(profile.source_format, SourceFormat::Cfg);
        assert_eq!(profile.config.len(), 2);
    }
}
