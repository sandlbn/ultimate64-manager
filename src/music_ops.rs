use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::music_player::{
    BrowserEntry, BrowserEntryType, MusicFileType, PlaylistEntry, SavedPlaylist,
};
use crate::net_utils::REST_TIMEOUT_SECS;
use crate::sid_info;

/// MD5 hash size for song length database lookups
pub const MD5_HASH_SIZE: usize = 16;

pub async fn play_music_file(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    song_number: Option<u8>,
    file_type: MusicFileType,
) -> Result<(), String> {
    log::info!("Playing: {} (song: {:?})", path.display(), song_number);

    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            match file_type {
                MusicFileType::Sid => conn.sid_play(&data, song_number).map_err(|e| e.to_string()),
                MusicFileType::Mod => conn.mod_play(&data).map_err(|e| e.to_string()),
                MusicFileType::Prg => conn.run_prg(&data).map_err(|e| e.to_string()),
            }
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Play timed out - device may be offline".to_string()),
    }
}

pub async fn download_song_lengths_async() -> Result<String, String> {
    let urls = [
        "https://hvsc.perv.dk/HVSC/C64Music/DOCUMENTS/Songlengths.md5",
        "http://hvsc.brona.dk/HVSC/C64Music/DOCUMENTS/Songlengths.md5",
    ];

    let config_dir = dirs::config_dir()
        .ok_or("Cannot determine config directory")?
        .join("ultimate64-manager");

    tokio::fs::create_dir_all(&config_dir)
        .await
        .map_err(|e| format!("Cannot create config dir: {}", e))?;

    let dest_path = config_dir.join("Songlengths.md5");

    let client = crate::net_utils::build_device_client(300)?;

    for url in urls {
        log::info!("Trying to download from: {}", url);

        match client.get(url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response
                        .bytes()
                        .await
                        .map_err(|e| format!("Download error: {}", e))?;

                    tokio::fs::write(&dest_path, &bytes)
                        .await
                        .map_err(|e| format!("Write error: {}", e))?;

                    return Ok(dest_path.to_string_lossy().to_string());
                }
            }
            Err(e) => {
                log::warn!("Failed to download from {}: {}", url, e);
                continue;
            }
        }
    }

    Err("All download attempts failed".to_string())
}

pub async fn parse_song_lengths_async(
    path: PathBuf,
) -> Result<HashMap<[u8; MD5_HASH_SIZE], Vec<u32>>, String> {
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Cannot read file: {}", e))?;

    let mut db: HashMap<[u8; MD5_HASH_SIZE], Vec<u32>> = HashMap::new();
    let mut count = 0;

    for line in content.lines() {
        let line = line.trim();

        if line.is_empty()
            || line.starts_with(';')
            || line.starts_with('#')
            || line.starts_with('[')
        {
            continue;
        }

        if let Some(eq_pos) = line.find('=') {
            let md5_str = &line[..eq_pos];
            let lengths_str = &line[eq_pos + 1..];

            if md5_str.len() != 32 {
                continue;
            }

            if let Some(hash) = sid_info::hex_to_md5(md5_str) {
                let mut lengths = Vec::new();

                for token in lengths_str.split_whitespace() {
                    if let Some(duration) = sid_info::parse_time_string(token) {
                        lengths.push(duration + 1);
                    }
                }

                if !lengths.is_empty() {
                    db.insert(hash, lengths);
                    count += 1;
                }
            }
        }
    }

    log::info!(
        "Parsed {} song length entries from {}",
        count,
        path.display()
    );

    Ok(db)
}

pub async fn save_playlist_async(playlist: SavedPlaylist) -> Result<String, String> {
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Playlist", &["json"])
        .set_file_name(&format!("{}.json", playlist.name))
        .save_file()
        .await
        .ok_or("Save cancelled")?;

    let path = handle.path().to_path_buf();

    let json = serde_json::to_string_pretty(&playlist)
        .map_err(|e| format!("Serialization error: {}", e))?;

    tokio::fs::write(&path, json)
        .await
        .map_err(|e| format!("Write error: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

pub async fn load_playlist_async() -> Result<Vec<PlaylistEntry>, String> {
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("Playlist", &["json"])
        .pick_file()
        .await
        .ok_or("Load cancelled")?;

    let path = handle.path().to_path_buf();

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Read error: {}", e))?;

    let playlist: SavedPlaylist =
        serde_json::from_str(&content).map_err(|e| format!("Parse error: {}", e))?;

    Ok(playlist.entries)
}

/// Search for music files (SID, MOD, PRG) recursively under `root`, matching
/// filenames or directory names against `query` (case-insensitive).
/// Returns BrowserEntry items with names showing relative paths from root.
pub fn search_files_recursive(root: &Path, query: &str) -> Vec<BrowserEntry> {
    let mut results: Vec<BrowserEntry> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if name.starts_with('.') {
                continue;
            }

            if path.is_dir() {
                stack.push(path.clone());

                if name.to_lowercase().contains(query) {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    let display_name = format!("[DIR] {}", rel.display());
                    results.push(BrowserEntry {
                        path,
                        name: display_name,
                        entry_type: BrowserEntryType::Directory,
                        subsongs: 1,
                        sid_tooltip: None,
                    });
                }
            } else if let Some(extension) = path.extension() {
                if let Some(ext_str) = extension.to_str() {
                    let ext_lower = ext_str.to_lowercase();
                    let file_type = match ext_lower.as_str() {
                        "sid" => Some(MusicFileType::Sid),
                        "mod" => Some(MusicFileType::Mod),
                        "prg" => Some(MusicFileType::Prg),
                        _ => None,
                    };

                    if let Some(ft) = file_type {
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        let rel_str = rel.to_string_lossy().to_lowercase();

                        if rel_str.contains(query) {
                            let (subsongs, sid_tooltip) = if ft == MusicFileType::Sid {
                                match fs::read(&path)
                                    .ok()
                                    .and_then(|data| sid_info::parse_header(&data).ok())
                                {
                                    Some(header) => {
                                        let songs = if header.songs > 0 && header.songs <= 256 {
                                            header.songs as u8
                                        } else {
                                            1
                                        };
                                        let mut tip = Vec::new();
                                        if !header.name.is_empty() {
                                            tip.push(header.name.clone());
                                        }
                                        if !header.author.is_empty() {
                                            tip.push(header.author.clone());
                                        }
                                        if !header.released.is_empty() {
                                            tip.push(format!("© {}", header.released));
                                        }
                                        tip.push(format!(
                                            "{} | {} | {} tunes",
                                            header.video_standard(),
                                            header.sid_model_info(),
                                            songs
                                        ));
                                        (songs, Some(tip.join("\n")))
                                    }
                                    None => (sid_info::quick_subsong_count(&path), None),
                                }
                            } else {
                                (1, None)
                            };

                            let display_name = rel.display().to_string();

                            results.push(BrowserEntry {
                                path,
                                name: display_name,
                                entry_type: BrowserEntryType::MusicFile(ft),
                                subsongs,
                                sid_tooltip,
                            });
                        }
                    }
                }
            }
        }
    }

    results.sort_by(|a, b| {
        let a_is_dir = matches!(a.entry_type, BrowserEntryType::Directory);
        let b_is_dir = matches!(b.entry_type, BrowserEntryType::Directory);
        match (a_is_dir, b_is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    results
}

pub fn format_total_duration(entries: &[PlaylistEntry], default_duration: u32) -> String {
    let total_seconds: u32 = entries
        .iter()
        .map(|e| e.duration.unwrap_or(default_duration))
        .sum();

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else {
        format!("{}m {}s", minutes, seconds)
    }
}
