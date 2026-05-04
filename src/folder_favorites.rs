//! Tiny JSON persistence for "favorite folders" used by the local and remote
//! file browsers. Both browsers store a `Vec<T>` of favorites — `PathBuf`
//! for the local side, `String` for remote device paths — under
//! `~/.config/ultimate64-manager/`.
//!
//! Failures (missing file, malformed JSON, no config dir) silently degrade to
//! an empty list. Favorites are user-comfort, not load-bearing — the browsers
//! must work without them.

use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

fn config_path(file_name: &str) -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("ultimate64-manager")
            .join(file_name),
    )
}

/// Read a saved favorites list. Returns an empty Vec on any failure so the
/// caller doesn't need to worry about the error path on first run.
pub fn load<T: DeserializeOwned>(file_name: &str) -> Vec<T> {
    let Some(path) = config_path(file_name) else {
        return Vec::new();
    };
    let Ok(s) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&s).unwrap_or_default()
}

/// Persist favorites. Errors are logged but never bubbled — the user's
/// in-memory list still works for the rest of the session.
pub fn save<T: Serialize>(file_name: &str, favorites: &[T]) {
    let Some(path) = config_path(file_name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(favorites) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&path, s) {
                log::warn!("Could not write {}: {}", path.display(), e);
            }
        }
        Err(e) => log::warn!("Could not serialize favorites for {}: {}", file_name, e),
    }
}
