use crate::mod_info;
use crate::net_utils::REST_TIMEOUT_SECS;
use crate::sid_info;
use iced::{
    widget::{
        button, column, container, progress_bar, row, rule, scrollable, text, text_input, tooltip,
        Column, Space,
    },
    Element, Length, Subscription, Task,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use ultimate64::Rest;

use crate::music_ops;

// MD5 hash size
const MD5_HASH_SIZE: usize = 16;
const DEFAULT_SONG_DURATION: u32 = 180; // 3 minutes default

/// SID metadata extracted from header, stored with playlist entries for display.
/// Provides rich info like PAL/NTSC, multi-SID chip addresses, and release year.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SidMetadata {
    pub title: String,
    pub author: String,
    pub released: String,
    pub video_std: String, // "PAL" or "NTSC"
    pub num_sids: usize,   // 1, 2, or 3
    pub sid_info: String,  // e.g. "1xSID" or "2xSID @ $D420"
}

impl SidMetadata {
    /// Build SidMetadata from a parsed SID file header.
    fn from_header(header: &sid_info::SidHeader) -> Self {
        Self {
            title: header.name.clone(),
            author: header.author.clone(),
            released: header.released.clone(),
            video_std: header.video_standard().to_string(),
            num_sids: header.num_sids(),
            sid_info: header.sid_model_info(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum MusicPlayerMessage {
    // Playback controls
    Play,
    Pause,
    Stop,
    NextSubsong,     // Next subsong within current file
    PreviousSubsong, // Previous subsong within current file
    NextFile,        // Next file in playlist
    PreviousFile,    // Previous file in playlist
    ToggleShuffle,
    ToggleRepeat,

    // File browser
    SelectDirectory,
    DirectorySelected(PathBuf),
    NavigateToDirectory(PathBuf),
    NavigateUp,
    RefreshBrowser,
    BrowserItemClicked(usize), // Click on browser item (double-click plays)
    BrowserFilterChanged(String), // Filter browser entries
    BrowserSearch,             // Recursive search in current dir and subdirs
    BrowserSearchComplete(Vec<BrowserEntry>), // Search results arrived
    BrowserClearSearch,        // Return to normal directory browsing

    // Playlist management
    AddToPlaylist(usize),      // Add from browser by index
    AddAndPlay(usize),         // Add and immediately play
    AddAllToPlaylist,          // Add all music files from current directory
    RemoveFromPlaylist(usize), // Remove from playlist by index
    ClearPlaylist,
    PlaylistItemSelected(usize),    // Select item in playlist
    PlaylistItemDoubleClick(usize), // Play item immediately
    MovePlaylistItemUp(usize),
    MovePlaylistItemDown(usize),

    // Playlist save/load
    SavePlaylist,
    LoadPlaylist,
    PlaylistSaved(Result<String, String>),
    PlaylistLoaded(Result<Vec<PlaylistEntry>, String>),
    PlaylistNameChanged(String),

    // Song length database
    DownloadSongLengths,
    SongLengthsDownloaded(Result<String, String>),
    LoadSongLengthsFromFile,
    SongLengthsLoaded(Result<HashMap<[u8; MD5_HASH_SIZE], Vec<u32>>, String>),
    SongLengthProgress(String),

    // Playback
    PlayFile(PathBuf, Option<u8>),
    PlaybackCompleted(Result<(), String>),
    SetSongNumber(u8),

    // Timer
    TimerTick,
    SongEnded,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub path: PathBuf,
    pub name: String, // Display name (from SID header or filename)
    pub file_type: MusicFileType,
    pub duration: Option<u32>, // Duration in seconds
    pub subsong: u8,
    pub max_subsongs: u8, // Total subsongs in this file
    #[serde(skip)]
    pub md5_hash: Option<[u8; MD5_HASH_SIZE]>,
    /// Rich SID metadata for display (None for MOD/PRG)
    #[serde(default)]
    pub sid_metadata: Option<SidMetadata>,
}

#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub name: String,
    pub entry_type: BrowserEntryType,
    pub subsongs: u8, // Number of subsongs (1 for non-SID or single-song SID)
    /// Tooltip text with SID header info (None for directories/non-SID)
    pub sid_tooltip: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BrowserEntryType {
    Directory,
    MusicFile(MusicFileType),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MusicFileType {
    Sid,
    Mod,
    Prg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlaylist {
    pub name: String,
    pub entries: Vec<PlaylistEntry>,
}

pub struct MusicPlayer {
    // Browser (left pane)
    browser_directory: PathBuf,
    browser_entries: Vec<BrowserEntry>,
    browser_selected: Option<usize>, // For double-click detection
    browser_filter: String,
    browser_search_active: bool, // True when showing recursive search results

    // Playlist (right pane)
    playlist: Vec<PlaylistEntry>,
    playlist_selected: Option<usize>,
    current_playing: Option<usize>,
    playlist_name: String,

    // Playback state
    pub playback_state: PlaybackState,
    shuffle_enabled: bool,
    repeat_enabled: bool,
    current_subsong: u8,
    max_subsongs: u8,
    shuffle_order: Vec<usize>,

    // Timer
    elapsed_seconds: u32,
    current_song_duration: u32,
    default_song_duration: u32, // Configurable default for unknown song lengths

    // Song length database
    song_lengths: HashMap<[u8; MD5_HASH_SIZE], Vec<u32>>,
    song_lengths_loaded: bool,
    song_lengths_status: String,

    // Status
    status_message: String,
}

impl MusicPlayer {
    /// Create a new MusicPlayer with an optional starting directory.
    /// If start_dir is None or invalid, defaults to the user's home directory.
    pub fn new(start_dir: Option<PathBuf>) -> Self {
        // Use provided path if it exists and is a directory, otherwise fall back to home
        let initial_dir = start_dir
            .filter(|p| p.exists() && p.is_dir())
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));

        let mut player = Self {
            browser_directory: initial_dir.clone(),
            browser_entries: Vec::new(),
            browser_selected: None,
            browser_filter: String::new(),
            browser_search_active: false,

            playlist: Vec::new(),
            playlist_selected: None,
            current_playing: None,
            playlist_name: "My Playlist".to_string(),

            playback_state: PlaybackState::Stopped,
            shuffle_enabled: false,
            repeat_enabled: false,
            current_subsong: 1,
            max_subsongs: 1,
            shuffle_order: Vec::new(),

            elapsed_seconds: 0,
            current_song_duration: DEFAULT_SONG_DURATION,
            default_song_duration: DEFAULT_SONG_DURATION,

            song_lengths: HashMap::new(),
            song_lengths_loaded: false,
            song_lengths_status: "Song lengths not loaded".to_string(),

            status_message: "Ready".to_string(),
        };

        player.load_browser_entries(&initial_dir);

        // Try to auto-load song lengths database from config directory
        if let Some(config_dir) = dirs::config_dir() {
            let db_path = config_dir
                .join("ultimate64-manager")
                .join("Songlengths.md5");
            if db_path.exists() {
                log::info!("Found song lengths database at {:?}", db_path);
                if let Ok(content) = fs::read_to_string(&db_path) {
                    let mut db: HashMap<[u8; MD5_HASH_SIZE], Vec<u32>> = HashMap::new();
                    let mut count = 0;

                    for line in content.lines() {
                        let line = line.trim();
                        // Skip empty lines, comments, and section headers like [Database]
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

                    if count > 0 {
                        player.song_lengths = db;
                        player.song_lengths_loaded = true;
                        player.song_lengths_status = format!("{} entries", count);
                        log::info!("Auto-loaded {} song length entries", count);
                    }
                }
            }
        }

        player
    }

    pub fn update(
        &mut self,
        message: MusicPlayerMessage,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Task<MusicPlayerMessage> {
        match message {
            // === Playback Controls ===
            MusicPlayerMessage::Play => {
                // If paused, just update state (main.rs will call resume API)
                if self.playback_state == PlaybackState::Paused {
                    if let Some(idx) = self.current_playing {
                        if let Some(entry) = self.playlist.get(idx) {
                            self.playback_state = PlaybackState::Playing;

                            let now_playing = if entry.name.is_empty() {
                                entry
                                    .path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "Unknown".to_string())
                            } else {
                                entry.name.clone()
                            };
                            self.status_message = format!("Playing: {}", now_playing);

                            return Task::none();
                        }
                    }
                }

                // Normal play - start playing a file
                if let Some(idx) = self.current_playing {
                    if let Some(entry) = self.playlist.get(idx) {
                        self.elapsed_seconds = 0;
                        self.playback_state = PlaybackState::Playing;
                        self.max_subsongs = entry.max_subsongs;

                        // Get duration from song length database or use default
                        self.current_song_duration = self.get_song_duration(entry);

                        let now_playing = if entry.name.is_empty() {
                            entry
                                .path
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| "Unknown".to_string())
                        } else {
                            entry.name.clone()
                        };
                        self.status_message = format!("Playing: {}", now_playing);

                        if let Some(conn) = connection {
                            let path = entry.path.clone();
                            let subsong = self.current_subsong;
                            let file_type = entry.file_type.clone();
                            return Task::perform(
                                play_music_file(conn, path, Some(subsong), file_type),
                                MusicPlayerMessage::PlaybackCompleted,
                            );
                        }
                    }
                } else if !self.playlist.is_empty() {
                    self.current_playing = Some(0);
                    self.current_subsong = 1;
                    self.elapsed_seconds = 0;
                    return self.update(MusicPlayerMessage::Play, connection);
                }
                Task::none()
            }

            MusicPlayerMessage::Pause => {
                self.playback_state = PlaybackState::Paused;
                self.status_message = "Paused".to_string();
                // Note: main.rs intercepts this and calls the machine pause API
                Task::none()
            }

            MusicPlayerMessage::Stop => {
                self.playback_state = PlaybackState::Stopped;
                self.elapsed_seconds = 0;
                self.current_subsong = 1;
                self.status_message = "Stopped".to_string();

                // Reset the Commodore to stop SID playback
                if let Some(conn) = connection {
                    return Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.reset().map_err(|e| e.to_string())
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(inner)) => inner,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => {
                                    Err("Reset timed out - device may be offline".to_string())
                                }
                            }
                        },
                        |result| {
                            if let Err(e) = result {
                                log::error!("Reset failed: {}", e);
                            }
                            MusicPlayerMessage::PlaybackCompleted(Ok(()))
                        },
                    );
                }
                Task::none()
            }

            MusicPlayerMessage::NextSubsong => {
                // Go to next subsong within current file
                if self.current_subsong < self.max_subsongs {
                    self.current_subsong += 1;
                    self.elapsed_seconds = 0;

                    // Update duration for this subsong from database
                    if let Some(idx) = self.current_playing {
                        if let Some(entry) = self.playlist.get(idx) {
                            if let Some(hash) = &entry.md5_hash {
                                if let Some(lengths) = self.song_lengths.get(hash) {
                                    // subsong is 1-based, array is 0-based
                                    let subsong_idx =
                                        (self.current_subsong as usize).saturating_sub(1);
                                    if subsong_idx < lengths.len() {
                                        self.current_song_duration = lengths[subsong_idx];
                                    }
                                }
                            }
                        }
                    }

                    if self.playback_state == PlaybackState::Playing {
                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::PreviousSubsong => {
                // Go to previous subsong within current file
                if self.current_subsong > 1 {
                    self.current_subsong -= 1;
                    self.elapsed_seconds = 0;

                    // Update duration for this subsong from database
                    if let Some(idx) = self.current_playing {
                        if let Some(entry) = self.playlist.get(idx) {
                            if let Some(hash) = &entry.md5_hash {
                                if let Some(lengths) = self.song_lengths.get(hash) {
                                    // subsong is 1-based, array is 0-based
                                    let subsong_idx =
                                        (self.current_subsong as usize).saturating_sub(1);
                                    if subsong_idx < lengths.len() {
                                        self.current_song_duration = lengths[subsong_idx];
                                    }
                                }
                            }
                        }
                    }

                    if self.playback_state == PlaybackState::Playing {
                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::NextFile => {
                self.next_track();
                if self.current_playing.is_some() {
                    self.elapsed_seconds = 0;
                    self.current_subsong = 1;
                    if self.playback_state == PlaybackState::Playing {
                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::PreviousFile => {
                self.previous_track();
                if self.current_playing.is_some() {
                    self.elapsed_seconds = 0;
                    self.current_subsong = 1;
                    if self.playback_state == PlaybackState::Playing {
                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::ToggleShuffle => {
                self.shuffle_enabled = !self.shuffle_enabled;
                if self.shuffle_enabled {
                    self.generate_shuffle_order();
                }
                Task::none()
            }

            MusicPlayerMessage::ToggleRepeat => {
                self.repeat_enabled = !self.repeat_enabled;
                Task::none()
            }

            // === File Browser ===
            MusicPlayerMessage::SelectDirectory => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                |result| {
                    if let Some(path) = result {
                        MusicPlayerMessage::DirectorySelected(path)
                    } else {
                        MusicPlayerMessage::RefreshBrowser
                    }
                },
            ),

            MusicPlayerMessage::DirectorySelected(path) => {
                self.browser_directory = path.clone();
                self.browser_selected = None;
                self.browser_search_active = false;
                self.load_browser_entries(&path);
                Task::none()
            }

            MusicPlayerMessage::NavigateToDirectory(path) => {
                self.browser_directory = path.clone();
                self.browser_selected = None;
                self.browser_search_active = false;
                self.load_browser_entries(&path);
                Task::none()
            }

            MusicPlayerMessage::NavigateUp => {
                if let Some(parent) = self.browser_directory.parent() {
                    let parent = parent.to_path_buf();
                    self.browser_directory = parent.clone();
                    self.browser_selected = None;
                    self.browser_search_active = false;
                    self.load_browser_entries(&parent);
                }
                Task::none()
            }

            MusicPlayerMessage::RefreshBrowser => {
                self.browser_selected = None;
                self.browser_search_active = false;
                self.load_browser_entries(&self.browser_directory.clone());
                Task::none()
            }

            MusicPlayerMessage::BrowserFilterChanged(value) => {
                self.browser_filter = value;
                Task::none()
            }

            MusicPlayerMessage::BrowserSearch => {
                let query = self.browser_filter.trim().to_lowercase();
                if query.is_empty() {
                    self.status_message = "Enter a search term first".to_string();
                    return Task::none();
                }
                self.status_message = format!("Searching for \"{}\"...", self.browser_filter);
                let root = self.browser_directory.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || search_files_recursive(&root, &query))
                            .await
                            .unwrap_or_default()
                    },
                    MusicPlayerMessage::BrowserSearchComplete,
                )
            }

            MusicPlayerMessage::BrowserSearchComplete(results) => {
                let count = results
                    .iter()
                    .filter(|e| matches!(e.entry_type, BrowserEntryType::MusicFile(_)))
                    .count();
                self.browser_entries = results;
                self.browser_selected = None;
                self.browser_search_active = true;
                self.status_message = format!("Found {} music files", count);
                Task::none()
            }

            MusicPlayerMessage::BrowserClearSearch => {
                self.browser_search_active = false;
                self.browser_selected = None;
                self.load_browser_entries(&self.browser_directory.clone());
                self.status_message = "Ready".to_string();
                Task::none()
            }

            MusicPlayerMessage::BrowserItemClicked(index) => {
                // Double-click detection: if same item clicked again, play it
                if let Some(browser_entry) = self.browser_entries.get(index) {
                    match &browser_entry.entry_type {
                        BrowserEntryType::Directory => {
                            // Navigate into directory
                            let path = browser_entry.path.clone();
                            return self
                                .update(MusicPlayerMessage::NavigateToDirectory(path), connection);
                        }
                        BrowserEntryType::MusicFile(_) => {
                            // Check if this is a double-click (same item selected)
                            if self.browser_selected == Some(index) {
                                // Double-click - add and play
                                return self
                                    .update(MusicPlayerMessage::AddAndPlay(index), connection);
                            } else {
                                // First click - just select
                                self.browser_selected = Some(index);
                            }
                        }
                    }
                }
                Task::none()
            }

            // === Playlist Management ===
            MusicPlayerMessage::AddToPlaylist(index) => {
                if let Some(browser_entry) = self.browser_entries.get(index) {
                    if let BrowserEntryType::MusicFile(ref ft) = browser_entry.entry_type {
                        let entry = self.create_playlist_entry(browser_entry, ft.clone());
                        let name = entry.name.clone();
                        self.playlist.push(entry);
                        self.status_message = format!("Added: {}", name);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::AddAndPlay(index) => {
                if let Some(browser_entry) = self.browser_entries.get(index) {
                    if let BrowserEntryType::MusicFile(ref ft) = browser_entry.entry_type {
                        let entry = self.create_playlist_entry(browser_entry, ft.clone());
                        let name = entry.name.clone();
                        self.playlist.push(entry);

                        // Set to play the newly added track
                        let new_idx = self.playlist.len() - 1;
                        self.current_playing = Some(new_idx);
                        self.elapsed_seconds = 0;
                        self.current_subsong = 1;
                        self.playback_state = PlaybackState::Playing;
                        self.status_message = format!("Playing: {}", name);

                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::AddAllToPlaylist => {
                let mut added_count = 0;
                // Collect all music files from browser
                let music_entries: Vec<_> = self
                    .browser_entries
                    .iter()
                    .filter_map(|entry| {
                        if let BrowserEntryType::MusicFile(ref ft) = entry.entry_type {
                            Some((entry.clone(), ft.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();

                for (browser_entry, ft) in music_entries {
                    let entry = self.create_playlist_entry(&browser_entry, ft);
                    self.playlist.push(entry);
                    added_count += 1;
                }

                if added_count > 0 {
                    self.status_message = format!("Added {} files to playlist", added_count);
                } else {
                    self.status_message = "No music files in current directory".to_string();
                }
                Task::none()
            }

            MusicPlayerMessage::RemoveFromPlaylist(index) => {
                if index < self.playlist.len() {
                    let removed = self.playlist.remove(index);
                    self.status_message = format!("Removed: {}", removed.name);

                    // Adjust current_playing if needed
                    if let Some(current) = self.current_playing {
                        if index < current {
                            self.current_playing = Some(current - 1);
                        } else if index == current {
                            self.current_playing = None;
                            self.playback_state = PlaybackState::Stopped;
                        }
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::ClearPlaylist => {
                self.playlist.clear();
                self.current_playing = None;
                self.playlist_selected = None;
                self.playback_state = PlaybackState::Stopped;
                self.status_message = "Playlist cleared".to_string();
                Task::none()
            }

            MusicPlayerMessage::PlaylistItemSelected(index) => {
                self.playlist_selected = Some(index);
                Task::none()
            }

            MusicPlayerMessage::PlaylistItemDoubleClick(index) => {
                self.current_playing = Some(index);
                self.elapsed_seconds = 0;
                self.current_subsong = 1;
                self.playback_state = PlaybackState::Playing;
                self.update(MusicPlayerMessage::Play, connection)
            }

            MusicPlayerMessage::MovePlaylistItemUp(index) => {
                if index > 0 && index < self.playlist.len() {
                    self.playlist.swap(index, index - 1);
                    self.playlist_selected = Some(index - 1);

                    // Adjust current_playing
                    if let Some(current) = self.current_playing {
                        if current == index {
                            self.current_playing = Some(index - 1);
                        } else if current == index - 1 {
                            self.current_playing = Some(index);
                        }
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::MovePlaylistItemDown(index) => {
                if index + 1 < self.playlist.len() {
                    self.playlist.swap(index, index + 1);
                    self.playlist_selected = Some(index + 1);

                    // Adjust current_playing
                    if let Some(current) = self.current_playing {
                        if current == index {
                            self.current_playing = Some(index + 1);
                        } else if current == index + 1 {
                            self.current_playing = Some(index);
                        }
                    }
                }
                Task::none()
            }

            // === Playlist Save/Load ===
            MusicPlayerMessage::PlaylistNameChanged(name) => {
                self.playlist_name = name;
                Task::none()
            }

            MusicPlayerMessage::SavePlaylist => {
                let playlist = SavedPlaylist {
                    name: self.playlist_name.clone(),
                    entries: self.playlist.clone(),
                };

                Task::perform(
                    save_playlist_async(playlist),
                    MusicPlayerMessage::PlaylistSaved,
                )
            }

            MusicPlayerMessage::LoadPlaylist => {
                Task::perform(load_playlist_async(), MusicPlayerMessage::PlaylistLoaded)
            }

            MusicPlayerMessage::PlaylistSaved(result) => {
                match result {
                    Ok(path) => {
                        self.status_message = format!("Playlist saved: {}", path);
                    }
                    Err(e) => {
                        self.status_message = format!("Save failed: {}", e);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::PlaylistLoaded(result) => {
                match result {
                    Ok(entries) => {
                        self.playlist = entries;
                        self.current_playing = None;
                        self.playlist_selected = None;

                        // Re-calculate MD5 hashes and look up durations and subsong counts
                        for entry in &mut self.playlist {
                            if entry.file_type == MusicFileType::Sid {
                                if let Ok(data) = fs::read(&entry.path) {
                                    // Re-parse header for metadata (may not be in saved JSON)
                                    if entry.sid_metadata.is_none() {
                                        if let Ok(header) = sid_info::parse_header(&data) {
                                            entry.sid_metadata =
                                                Some(SidMetadata::from_header(&header));
                                        }
                                    }

                                    let hash = sid_info::compute_md5(&data);
                                    entry.md5_hash = Some(hash);

                                    // Song length database is authoritative
                                    if let Some(lengths) = self.song_lengths.get(&hash) {
                                        // Update subsong count from database
                                        if lengths.len() > entry.max_subsongs as usize
                                            && lengths.len() <= 256
                                        {
                                            entry.max_subsongs = lengths.len() as u8;
                                        }
                                        // Update duration for subsong 1
                                        if !lengths.is_empty() {
                                            entry.duration = Some(lengths[0]);
                                        }
                                    }
                                }
                            }
                        }

                        self.status_message =
                            format!("Loaded playlist with {} entries", self.playlist.len());
                    }
                    Err(e) => {
                        self.status_message = format!("Load failed: {}", e);
                    }
                }
                Task::none()
            }

            // === Song Length Database ===
            MusicPlayerMessage::DownloadSongLengths => {
                self.song_lengths_status = "Downloading song lengths database...".to_string();
                Task::perform(
                    download_song_lengths_async(),
                    MusicPlayerMessage::SongLengthsDownloaded,
                )
            }

            MusicPlayerMessage::SongLengthsDownloaded(result) => {
                match result {
                    Ok(path) => {
                        self.song_lengths_status = format!("Downloaded to: {}", path);
                        // Now load the file
                        return Task::perform(
                            parse_song_lengths_async(PathBuf::from(path)),
                            MusicPlayerMessage::SongLengthsLoaded,
                        );
                    }
                    Err(e) => {
                        self.song_lengths_status = format!("Download failed: {}", e);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::LoadSongLengthsFromFile => {
                self.song_lengths_status = "Select song lengths file...".to_string();
                Task::perform(
                    async {
                        if let Some(handle) = rfd::AsyncFileDialog::new()
                            .add_filter("MD5 Files", &["md5", "txt"])
                            .pick_file()
                            .await
                        {
                            let path = handle.path().to_path_buf();
                            parse_song_lengths_async(path).await
                        } else {
                            Err("No file selected".to_string())
                        }
                    },
                    MusicPlayerMessage::SongLengthsLoaded,
                )
            }

            MusicPlayerMessage::SongLengthsLoaded(result) => {
                match result {
                    Ok(db) => {
                        let count = db.len();
                        self.song_lengths = db;
                        self.song_lengths_loaded = true;
                        self.song_lengths_status = format!("{} entries", count);

                        // Update existing playlist entries with subsong counts and durations
                        let mut updated = 0;
                        for entry in &mut self.playlist {
                            if let Some(hash) = &entry.md5_hash {
                                if let Some(lengths) = self.song_lengths.get(hash) {
                                    // Database subsong count is authoritative
                                    if lengths.len() > entry.max_subsongs as usize
                                        && lengths.len() <= 256
                                    {
                                        entry.max_subsongs = lengths.len() as u8;
                                        updated += 1;
                                    }
                                    // Update duration for subsong 1
                                    if !lengths.is_empty() {
                                        entry.duration = Some(lengths[0]);
                                    }
                                }
                            }
                        }

                        if updated > 0 {
                            log::info!(
                                "Updated {} playlist entries with database subsong counts",
                                updated
                            );
                        }
                    }
                    Err(e) => {
                        self.song_lengths_status = format!("Load failed: {}", e);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::SongLengthProgress(msg) => {
                self.song_lengths_status = msg;
                Task::none()
            }

            // === Playback ===
            MusicPlayerMessage::PlayFile(path, song_num) => {
                if let Some(conn) = connection {
                    Task::perform(
                        play_music_file(conn, path, song_num, MusicFileType::Sid),
                        MusicPlayerMessage::PlaybackCompleted,
                    )
                } else {
                    Task::none()
                }
            }

            MusicPlayerMessage::PlaybackCompleted(result) => {
                if let Err(e) = result {
                    self.status_message = format!("Playback error: {}", e);
                    log::error!("Playback failed: {}", e);
                }
                Task::none()
            }

            MusicPlayerMessage::SetSongNumber(num) => {
                if num >= 1 && num <= self.max_subsongs {
                    self.current_subsong = num;
                    self.elapsed_seconds = 0;

                    // Update duration for this subsong
                    if let Some(idx) = self.current_playing {
                        if let Some(entry) = self.playlist.get(idx) {
                            if let Some(hash) = &entry.md5_hash {
                                if let Some(lengths) = self.song_lengths.get(hash) {
                                    let subsong_idx = (num as usize).saturating_sub(1);
                                    if subsong_idx < lengths.len() {
                                        self.current_song_duration = lengths[subsong_idx];
                                    }
                                }
                            }
                        }
                    }

                    if self.playback_state == PlaybackState::Playing {
                        return self.update(MusicPlayerMessage::Play, connection);
                    }
                }
                Task::none()
            }

            // === Timer ===
            MusicPlayerMessage::TimerTick => {
                if self.playback_state == PlaybackState::Playing {
                    self.elapsed_seconds += 1;

                    // Check if song should end
                    if self.elapsed_seconds >= self.current_song_duration {
                        return self.update(MusicPlayerMessage::SongEnded, connection);
                    }
                }
                Task::none()
            }

            MusicPlayerMessage::SongEnded => {
                // Check if there are more subsongs
                if self.current_subsong < self.max_subsongs {
                    self.current_subsong += 1;
                    self.elapsed_seconds = 0;

                    // Update duration for next subsong
                    if let Some(idx) = self.current_playing {
                        if let Some(entry) = self.playlist.get(idx) {
                            if let Some(hash) = &entry.md5_hash {
                                if let Some(lengths) = self.song_lengths.get(hash) {
                                    let subsong_idx =
                                        (self.current_subsong as usize).saturating_sub(1);
                                    if subsong_idx < lengths.len() {
                                        self.current_song_duration = lengths[subsong_idx];
                                    }
                                }
                            }
                        }
                    }

                    return self.update(MusicPlayerMessage::Play, connection);
                }

                // Check if current song is a MOD (needs reset to stop looping)
                let was_mod = self
                    .current_playing
                    .and_then(|idx| self.playlist.get(idx))
                    .map(|entry| matches!(entry.file_type, MusicFileType::Mod | MusicFileType::Prg))
                    .unwrap_or(false);

                // Move to next track
                self.next_track();

                if self.current_playing.is_some() {
                    self.elapsed_seconds = 0;
                    self.current_subsong = 1;

                    // If previous song was MOD, reset first then play next
                    if was_mod {
                        if let Some(conn) = connection {
                            let conn_clone = conn.clone();
                            return Task::perform(
                                async move {
                                    let result = tokio::time::timeout(
                                        tokio::time::Duration::from_secs(2),
                                        tokio::task::spawn_blocking(move || {
                                            let c = conn_clone.blocking_lock();
                                            c.reset().map_err(|e| e.to_string())
                                        }),
                                    )
                                    .await;

                                    match result {
                                        Ok(Ok(_)) => Ok(()),
                                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                        Err(_) => Err("Reset timed out".to_string()),
                                    }
                                },
                                |result| {
                                    if let Err(e) = result {
                                        log::error!("MOD reset failed: {}", e);
                                    }
                                    // Play next song after reset
                                    MusicPlayerMessage::Play
                                },
                            );
                        }
                    }

                    self.update(MusicPlayerMessage::Play, connection)
                } else {
                    // End of playlist
                    if self.repeat_enabled && !self.playlist.is_empty() {
                        self.current_playing = Some(0);
                        self.elapsed_seconds = 0;
                        self.current_subsong = 1;

                        // Reset before repeating if last was MOD
                        if was_mod {
                            if let Some(conn) = connection {
                                let conn_clone = conn.clone();
                                return Task::perform(
                                    async move {
                                        let result = tokio::time::timeout(
                                            tokio::time::Duration::from_secs(2),
                                            tokio::task::spawn_blocking(move || {
                                                let c = conn_clone.blocking_lock();
                                                c.reset().map_err(|e| e.to_string())
                                            }),
                                        )
                                        .await;

                                        match result {
                                            Ok(Ok(_)) => Ok(()),
                                            Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                            Err(_) => Err("Reset timed out".to_string()),
                                        }
                                    },
                                    |result| {
                                        if let Err(e) = result {
                                            log::error!("MOD reset failed: {}", e);
                                        }
                                        MusicPlayerMessage::Play
                                    },
                                );
                            }
                        }

                        self.update(MusicPlayerMessage::Play, connection)
                    } else {
                        // Playlist ended - stop playback on hardware
                        self.playback_state = PlaybackState::Stopped;
                        self.status_message = "Playlist ended".to_string();

                        // Reset the C64 to stop MOD/SID playback
                        if let Some(conn) = connection {
                            return Task::perform(
                                async move {
                                    let result = tokio::time::timeout(
                                        tokio::time::Duration::from_secs(2),
                                        tokio::task::spawn_blocking(move || {
                                            let conn = conn.blocking_lock();
                                            conn.reboot().map_err(|e| e.to_string())
                                        }),
                                    )
                                    .await;

                                    match result {
                                        Ok(Ok(inner)) => inner,
                                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                        Err(_) => Err("Reset timed out".to_string()),
                                    }
                                },
                                |result| {
                                    if let Err(e) = result {
                                        log::error!("Reset failed: {}", e);
                                    }
                                    MusicPlayerMessage::PlaybackCompleted(Ok(()))
                                },
                            );
                        }

                        Task::none()
                    }
                }
            }
        }
    }

    pub fn view(&self, font_size: u32) -> Element<'_, MusicPlayerMessage> {
        let small = (font_size.saturating_sub(2)).max(8);
        let normal = font_size;
        let large = font_size + 2;
        let header = font_size + 4;

        // === TOP: Now playing info ===
        let (now_playing_text, now_playing_meta) = if let Some(idx) = self.current_playing {
            if let Some(entry) = self.playlist.get(idx) {
                let icon = match entry.file_type {
                    MusicFileType::Sid => "[SID]",
                    MusicFileType::Mod => "[MOD]",
                    MusicFileType::Prg => "[PRG]",
                };
                let name = if entry.name.is_empty() {
                    entry
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Unknown".to_string())
                } else {
                    entry.name.clone()
                };

                // Build metadata line for SID files: "PAL | 2xSID @ $D420 | 11 tunes | © 1988"
                let meta = if let Some(ref m) = entry.sid_metadata {
                    let mut parts = Vec::new();
                    parts.push(m.video_std.clone());
                    parts.push(m.sid_info.clone());
                    if entry.max_subsongs > 1 {
                        parts.push(format!("{} tunes", entry.max_subsongs));
                    }
                    if !m.released.is_empty() {
                        parts.push(format!("© {}", m.released));
                    }
                    parts.join(" | ")
                } else {
                    String::new()
                };

                (format!("{} {}", icon, name), meta)
            } else {
                ("No track selected".to_string(), String::new())
            }
        } else {
            ("No track selected".to_string(), String::new())
        };

        let now_playing = text(now_playing_text.clone()).size(large);

        // Time display
        let remaining = self
            .current_song_duration
            .saturating_sub(self.elapsed_seconds);
        let time_display = text(format!(
            "{}:{:02} / {}:{:02} (-{}:{:02})",
            self.elapsed_seconds / 60,
            self.elapsed_seconds % 60,
            self.current_song_duration / 60,
            self.current_song_duration % 60,
            remaining / 60,
            remaining % 60
        ))
        .size(normal);

        // Transport buttons - separate file and subsong navigation
        let transport = row![
            // File navigation
            tooltip(
                button(text("|<").size(normal))
                    .on_press(MusicPlayerMessage::PreviousFile)
                    .padding([4, 8]),
                "Previous file in playlist",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            // Subsong navigation
            tooltip(
                button(text("<<").size(normal))
                    .on_press(MusicPlayerMessage::PreviousSubsong)
                    .padding([4, 6]),
                "Previous subsong within current file",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(
                    text(if self.playback_state == PlaybackState::Playing {
                        "‖"
                    } else {
                        "▶"
                    })
                    .size(normal)
                )
                .on_press(if self.playback_state == PlaybackState::Playing {
                    MusicPlayerMessage::Pause
                } else {
                    MusicPlayerMessage::Play
                })
                .padding([4, 12]),
                if self.playback_state == PlaybackState::Playing {
                    "Pause playback"
                } else {
                    "Start or resume playback"
                },
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(text("■").size(normal))
                    .on_press(MusicPlayerMessage::Stop)
                    .padding([4, 8]),
                "Stop playback and reset to beginning",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            // Subsong navigation
            tooltip(
                button(text(">>").size(normal))
                    .on_press(MusicPlayerMessage::NextSubsong)
                    .padding([4, 6]),
                "Next subsong within current file",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            // File navigation
            tooltip(
                button(text(">|").size(normal))
                    .on_press(MusicPlayerMessage::NextFile)
                    .padding([4, 8]),
                "Next file in playlist",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            // Subsong indicator
            text(format!(
                "Tune {}/{}",
                self.current_subsong, self.max_subsongs
            ))
            .size(normal),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Mode toggles
        let modes = row![
            tooltip(
                button(
                    text(if self.shuffle_enabled {
                        "↭ Shuffle: ON"
                    } else {
                        "↭ Shuffle"
                    })
                    .size(small)
                )
                .on_press(MusicPlayerMessage::ToggleShuffle)
                .padding([3, 6])
                .style(if self.shuffle_enabled {
                    button::primary
                } else {
                    button::secondary
                }),
                "Randomize playlist order",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
            tooltip(
                button(
                    text(if self.repeat_enabled {
                        "⟳ Repeat: ON"
                    } else {
                        "⟳ Repeat"
                    })
                    .size(small)
                )
                .on_press(MusicPlayerMessage::ToggleRepeat)
                .padding([3, 6])
                .style(if self.repeat_enabled {
                    button::primary
                } else {
                    button::secondary
                }),
                "Repeat playlist when finished",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),
        ]
        .spacing(5);

        // Build the top bar with now-playing info and optional metadata line
        let mut top_bar_items: Vec<Element<'_, MusicPlayerMessage>> =
            vec![
                row![now_playing, Space::new().width(Length::Fill), time_display]
                    .align_y(iced::Alignment::Center)
                    .into(),
            ];

        // Show SID metadata line if available (PAL | 2xSID @ $D420 | © 1988)
        if !now_playing_meta.is_empty() {
            top_bar_items.push(
                text(now_playing_meta)
                    .size(small)
                    .color(iced::Color::from_rgb(0.6, 0.7, 0.8))
                    .into(),
            );
        }

        top_bar_items.push(
            row![transport, Space::new().width(20), modes]
                .align_y(iced::Alignment::Center)
                .into(),
        );

        // Progress bar
        top_bar_items.push(
            container(progress_bar(
                0.0..=self.current_song_duration as f32,
                self.elapsed_seconds as f32,
            ))
            .width(Length::Fill)
            .height(Length::Fixed(8.0))
            .into(),
        );

        let top_bar = Column::with_children(top_bar_items).spacing(8).padding(10);

        // === LEFT PANE: File Browser ===
        let dir_display = truncate_path(&self.browser_directory, 40);

        // Count music files
        let music_file_count = self
            .browser_entries
            .iter()
            .filter(|e| matches!(e.entry_type, BrowserEntryType::MusicFile(_)))
            .count();

        let browser_header = container(
            column![
                text(if self.browser_search_active {
                    "SEARCH RESULTS"
                } else {
                    "LOCAL FILES"
                })
                .size(normal),
                row![
                    tooltip(
                        button(text("Browse").size(small))
                            .on_press(MusicPlayerMessage::SelectDirectory)
                            .padding([3, 8]),
                        "Select a directory to browse",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        button(text("Up").size(small))
                            .on_press(MusicPlayerMessage::NavigateUp)
                            .padding([3, 8]),
                        "Go to parent directory",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        button(text("Refresh").size(small))
                            .on_press(MusicPlayerMessage::RefreshBrowser)
                            .padding([3, 8]),
                        "Refresh current directory listing",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        button(text("Add All").size(small))
                            .on_press(MusicPlayerMessage::AddAllToPlaylist)
                            .padding([3, 8]),
                        if self.browser_search_active {
                            "Add all found files to playlist"
                        } else {
                            "Add all music files from current directory to playlist"
                        },
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
                row![
                    text("Search:").size(small),
                    text_input("filename or directory...", &self.browser_filter)
                        .on_input(MusicPlayerMessage::BrowserFilterChanged)
                        .on_submit(MusicPlayerMessage::BrowserSearch)
                        .size(small)
                        .padding(4)
                        .width(Length::Fill),
                    tooltip(
                        button(text("Find").size(small))
                            .on_press(MusicPlayerMessage::BrowserSearch)
                            .padding([3, 8])
                            .style(button::primary),
                        "Search recursively in all subdirectories (Enter)",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    if self.browser_search_active {
                        tooltip(
                            button(text("Clear").size(small))
                                .on_press(MusicPlayerMessage::BrowserClearSearch)
                                .padding([3, 8]),
                            "Return to directory browsing",
                            tooltip::Position::Bottom,
                        )
                        .style(container::bordered_box)
                    } else {
                        // Invisible placeholder to keep layout stable
                        tooltip(
                            button(text("Clear").size(small)).padding([3, 8]),
                            "",
                            tooltip::Position::Bottom,
                        )
                        .style(container::bordered_box)
                    },
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
                text(dir_display.clone()).size(small),
                text(if self.browser_search_active {
                    format!("{} music files found", music_file_count)
                } else {
                    format!("{} music files", music_file_count)
                })
                .size(small),
            ]
            .spacing(5),
        )
        .padding(10);

        let browser_list: Element<'_, MusicPlayerMessage> = if self.browser_entries.is_empty() {
            container(
                text(if self.browser_search_active {
                    "No matching files found"
                } else {
                    "Empty directory"
                })
                .size(normal),
            )
            .padding(10)
            .into()
        } else {
            // Filter entries based on filter text (skip when showing search results)
            let filtered_entries: Vec<(usize, &BrowserEntry)> = self
                .browser_entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    self.browser_search_active
                        || self.browser_filter.is_empty()
                        || entry
                            .name
                            .to_lowercase()
                            .contains(&self.browser_filter.to_lowercase())
                })
                .collect();

            let items: Vec<Element<'_, MusicPlayerMessage>> = filtered_entries
                .iter()
                .map(|(idx, entry)| {
                    let is_selected = self.browser_selected == Some(*idx);

                    match &entry.entry_type {
                        BrowserEntryType::Directory => {
                            // Directory entry - click to navigate
                            row![button(text(format!("[DIR] {}", entry.name)).size(normal))
                                .on_press(MusicPlayerMessage::BrowserItemClicked(*idx))
                                .padding([6, 8])
                                .width(Length::Fill)
                                .style(button::text),]
                            .into()
                        }
                        BrowserEntryType::MusicFile(ft) => {
                            // Music file entry - show add and play buttons
                            let icon = match ft {
                                MusicFileType::Sid => "[SID]",
                                MusicFileType::Mod => "[MOD]",
                                MusicFileType::Prg => "[PRG]",
                            };

                            // Show subsong count for multi-subsong files
                            let subsong_info = if entry.subsongs > 1 {
                                format!(" ({})", entry.subsongs)
                            } else {
                                String::new()
                            };

                            let max_name_len = 40;
                            let display = truncate_string(&entry.name, max_name_len);
                            let is_truncated = entry.name.chars().count() > max_name_len;

                            let file_button = button(
                                text(format!("{}{} {}", icon, subsong_info, display)).size(normal),
                            )
                            .on_press(MusicPlayerMessage::BrowserItemClicked(*idx))
                            .padding([6, 8])
                            .width(Length::Fill)
                            .style(if is_selected {
                                button::primary
                            } else {
                                button::text
                            });

                            // Tooltip: SID info takes priority, include full name if truncated
                            let file_element: Element<'_, MusicPlayerMessage> =
                                if let Some(ref tip) = entry.sid_tooltip {
                                    // SID tooltip: show header metadata (author, title, PAL/NTSC, etc.)
                                    let tip_text = if is_truncated {
                                        format!("{}\n\n{}", entry.name, tip)
                                    } else {
                                        tip.clone()
                                    };
                                    tooltip(
                                        file_button,
                                        text(tip_text).size(small),
                                        tooltip::Position::Top,
                                    )
                                    .style(container::bordered_box)
                                    .into()
                                } else if is_truncated {
                                    // Just show full filename for truncated names
                                    tooltip(
                                        file_button,
                                        text(&entry.name).size(normal),
                                        tooltip::Position::Top,
                                    )
                                    .style(container::bordered_box)
                                    .into()
                                } else {
                                    file_button.into()
                                };

                            row![
                                file_element,
                                tooltip(
                                    button(text(">").size(small))
                                        .on_press(MusicPlayerMessage::AddAndPlay(*idx))
                                        .padding([4, 8]),
                                    "Add to playlist and play immediately",
                                    tooltip::Position::Bottom,
                                )
                                .style(container::bordered_box),
                                tooltip(
                                    button(text("+").size(small))
                                        .on_press(MusicPlayerMessage::AddToPlaylist(*idx))
                                        .padding([4, 8]),
                                    "Add to playlist",
                                    tooltip::Position::Bottom,
                                )
                                .style(container::bordered_box),
                            ]
                            .spacing(4)
                            .align_y(iced::Alignment::Center)
                            .into()
                        }
                    }
                })
                .collect();

            scrollable(
                Column::with_children(items)
                    .spacing(2)
                    .padding(iced::Padding::new(5.0).right(15.0)), // Extra right padding for scrollbar
            )
            .height(Length::Fill)
            .into()
        };

        let browser_pane = container(
            column![browser_header, rule::horizontal(1), browser_list]
                .spacing(0)
                .height(Length::Fill),
        )
        .width(Length::FillPortion(1));

        // === RIGHT PANE: Playlist ===
        let playlist_header = container(
            column![
                text("PLAYLIST").size(normal),
                row![
                    text_input("Playlist name", &self.playlist_name)
                        .on_input(MusicPlayerMessage::PlaylistNameChanged)
                        .size(small)
                        .width(Length::Fixed(120.0)),
                    tooltip(
                        button(text("Save").size(small))
                            .on_press(MusicPlayerMessage::SavePlaylist)
                            .padding([3, 6]),
                        "Save playlist to a JSON file",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        button(text("Load").size(small))
                            .on_press(MusicPlayerMessage::LoadPlaylist)
                            .padding([3, 6]),
                        "Load playlist from a JSON file",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    tooltip(
                        button(text("Clear").size(small))
                            .on_press(MusicPlayerMessage::ClearPlaylist)
                            .padding([3, 6]),
                        "Remove all items from playlist",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(5),
                text(format!(
                    "{} tracks | Total: {}",
                    self.playlist.len(),
                    format_total_duration(&self.playlist, self.default_song_duration)
                ))
                .size(small),
            ]
            .spacing(5),
        )
        .padding(10);

        let playlist_list: Element<'_, MusicPlayerMessage> = if self.playlist.is_empty() {
            container(text("Playlist is empty\nDouble-click files to add and play").size(normal))
                .padding(10)
                .into()
        } else {
            let items: Vec<Element<'_, MusicPlayerMessage>> = self
                .playlist
                .iter()
                .enumerate()
                .map(|(idx, entry)| {
                    let is_selected = self.playlist_selected == Some(idx);
                    let is_playing = self.current_playing == Some(idx);

                    let prefix = if is_playing {
                        match self.playback_state {
                            PlaybackState::Playing => ">",
                            PlaybackState::Paused => "=",
                            PlaybackState::Stopped => "*",
                        }
                    } else {
                        " "
                    };

                    // Build compact badge: [S NTSC 2SID x3] or [M] or [P]
                    // PAL is omitted (default), NTSC shown explicitly
                    let badge = match entry.file_type {
                        MusicFileType::Sid => {
                            let mut parts = vec!["S".to_string()];
                            if let Some(ref m) = entry.sid_metadata {
                                // Only show NTSC (PAL is default/assumed)
                                if m.video_std == "NTSC" {
                                    parts.push("NTSC".to_string());
                                }
                                if m.num_sids > 1 {
                                    parts.push(format!("{}SID", m.num_sids));
                                }
                            }
                            if entry.max_subsongs > 1 {
                                parts.push(format!("x{}", entry.max_subsongs));
                            }
                            parts.join(" ")
                        }
                        MusicFileType::Mod => "M".to_string(),
                        MusicFileType::Prg => "P".to_string(),
                    };

                    let duration_str = if let Some(dur) = entry.duration {
                        format!("{}:{:02}", dur / 60, dur % 60)
                    } else {
                        "3:00".to_string()
                    };

                    // Show parsed name or filename
                    let display_name = if entry.name.is_empty() {
                        entry
                            .path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Unknown".to_string())
                    } else {
                        entry.name.clone()
                    };
                    let max_name_len = 40;
                    let name = truncate_string(&display_name, max_name_len);
                    let is_truncated = display_name.chars().count() > max_name_len;

                    let tiny = (font_size.saturating_sub(3)).max(7);

                    let playlist_button = button(
                        text(format!(
                            "{} [{}] {} ({})",
                            prefix, badge, name, duration_str
                        ))
                        .size(small),
                    )
                    .on_press(MusicPlayerMessage::PlaylistItemSelected(idx))
                    .padding([6, 8])
                    .width(Length::Fill)
                    .style(if is_selected || is_playing {
                        button::primary
                    } else {
                        button::text
                    });

                    // Tooltip: show full name + SID metadata details
                    let playlist_element: Element<'_, MusicPlayerMessage> = {
                        let has_meta = entry.sid_metadata.is_some();
                        if is_truncated || has_meta {
                            let mut tip_parts = Vec::new();
                            if is_truncated {
                                tip_parts.push(display_name.clone());
                            }
                            if let Some(ref m) = entry.sid_metadata {
                                if is_truncated {
                                    tip_parts.push(String::new()); // blank line separator
                                }
                                tip_parts.push(format!(
                                    "{} | {} | {} tunes",
                                    m.video_std, m.sid_info, entry.max_subsongs
                                ));
                                if !m.released.is_empty() {
                                    tip_parts.push(format!("© {}", m.released));
                                }
                            }
                            tooltip(
                                playlist_button,
                                text(tip_parts.join("\n")).size(small),
                                tooltip::Position::Top,
                            )
                            .style(container::bordered_box)
                            .into()
                        } else {
                            playlist_button.into()
                        }
                    };

                    row![
                        playlist_element,
                        tooltip(
                            button(text("^").size(tiny))
                                .on_press(MusicPlayerMessage::MovePlaylistItemUp(idx))
                                .padding([4, 6]),
                            "Move up in playlist",
                            tooltip::Position::Bottom,
                        )
                        .style(container::bordered_box),
                        tooltip(
                            button(text("v").size(tiny))
                                .on_press(MusicPlayerMessage::MovePlaylistItemDown(idx))
                                .padding([4, 6]),
                            "Move down in playlist",
                            tooltip::Position::Bottom,
                        )
                        .style(container::bordered_box),
                        tooltip(
                            button(text("X").size(tiny))
                                .on_press(MusicPlayerMessage::RemoveFromPlaylist(idx))
                                .padding([4, 6]),
                            "Remove from playlist",
                            tooltip::Position::Bottom,
                        )
                        .style(container::bordered_box),
                    ]
                    .spacing(2)
                    .align_y(iced::Alignment::Center)
                    .into()
                })
                .collect();

            scrollable(
                Column::with_children(items)
                    .spacing(2)
                    .padding(iced::Padding::new(5.0).right(15.0)), // Extra right padding for scrollbar
            )
            .height(Length::Fill)
            .into()
        };

        // Play selected button
        let playlist_controls = container(if let Some(selected) = self.playlist_selected {
            row![tooltip(
                button(text("Play Selected").size(small))
                    .on_press(MusicPlayerMessage::PlaylistItemDoubleClick(selected))
                    .padding([4, 10]),
                "Start playing the selected track",
                tooltip::Position::Bottom,
            )
            .style(container::bordered_box),]
        } else {
            row![]
        })
        .padding([5, 10]);

        let playlist_pane = container(
            column![
                playlist_header,
                rule::horizontal(1),
                playlist_list,
                playlist_controls,
            ]
            .spacing(0)
            .height(Length::Fill),
        )
        .width(Length::FillPortion(1));

        // === BOTTOM: Song Length Database Controls ===
        let db_status = if self.song_lengths_loaded {
            format!("{} entries", self.song_lengths.len())
        } else {
            self.song_lengths_status.clone()
        };

        let db_controls = container(
            row![
                text("Song Lengths:").size(small),
                tooltip(
                    button(text("Download HVSC").size(small))
                        .on_press(MusicPlayerMessage::DownloadSongLengths)
                        .padding([3, 8]),
                    "Download song length database from HVSC\n(High Voltage SID Collection)",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("Load File").size(small))
                        .on_press(MusicPlayerMessage::LoadSongLengthsFromFile)
                        .padding([3, 8]),
                    "Load song length database from local file",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text(db_status.clone()).size(small),
                Space::new().width(Length::Fill),
                text(&self.status_message).size(small),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([5, 10]);

        // === Main Layout ===
        let main_content =
            row![browser_pane, rule::vertical(1), playlist_pane].height(Length::Fill);

        column![
            text("MUSIC PLAYER").size(header),
            rule::horizontal(1),
            top_bar,
            rule::horizontal(1),
            main_content,
            rule::horizontal(1),
            db_controls,
        ]
        .spacing(5)
        .padding(10)
        .into()
    }

    pub fn subscription(&self) -> Subscription<MusicPlayerMessage> {
        if self.playback_state == PlaybackState::Playing {
            iced::time::every(Duration::from_secs(1)).map(|_| MusicPlayerMessage::TimerTick)
        } else {
            Subscription::none()
        }
    }

    // Helper methods

    fn create_playlist_entry(
        &self,
        browser_entry: &BrowserEntry,
        file_type: MusicFileType,
    ) -> PlaylistEntry {
        let mut entry = PlaylistEntry {
            path: browser_entry.path.clone(),
            name: String::new(),
            file_type,
            duration: None,
            subsong: 1,
            max_subsongs: browser_entry.subsongs, // Start with SID header count
            md5_hash: None,
            sid_metadata: None,
        };

        // Parse SID header only when adding to playlist (lazy loading)
        if entry.file_type == MusicFileType::Sid {
            if let Ok(data) = fs::read(&entry.path) {
                // Parse SID header for display name
                if let Ok(header) = sid_info::parse_header(&data) {
                    let display = header.display_name();
                    entry.name = if display.is_empty() {
                        browser_entry.name.clone()
                    } else {
                        display
                    };
                    entry.max_subsongs = header.songs as u8;
                    // Store rich metadata for display in playlist and now-playing bar
                    entry.sid_metadata = Some(SidMetadata::from_header(&header));
                } else {
                    entry.name = browser_entry.name.clone();
                }

                // Calculate MD5 and look up in song length database
                let hash = sid_info::compute_md5(&data);
                entry.md5_hash = Some(hash);

                // Song length database is authoritative for subsong count and durations
                if let Some(lengths) = self.song_lengths.get(&hash) {
                    // Database subsong count is more reliable than SID header
                    if lengths.len() > entry.max_subsongs as usize && lengths.len() <= 256 {
                        entry.max_subsongs = lengths.len() as u8;
                    }
                    // Get duration for first subsong (subsong 1 = index 0)
                    if !lengths.is_empty() {
                        entry.duration = Some(lengths[0]);
                    }
                }
            } else {
                entry.name = browser_entry.name.clone();
            }
        }
        if entry.file_type == MusicFileType::Mod {
            if let Ok(data) = fs::read(&entry.path) {
                if let Ok(info) = mod_info::parse_mod(&data) {
                    entry.name = if let Some(author) = &info.author {
                        format!("{} - {}", author, info.name)
                    } else if !info.name.is_empty() {
                        info.name.clone()
                    } else {
                        browser_entry.name.clone()
                    };
                    entry.duration = Some(info.duration_seconds);
                }
            }
        } else if entry.file_type == MusicFileType::Prg {
            entry.name = browser_entry.name.clone();
            entry.duration = Some(self.default_song_duration);
        } else {
            entry.name = browser_entry.name.clone();
        }

        entry
    }

    fn load_browser_entries(&mut self, directory: &Path) {
        self.browser_entries.clear();

        // Read directory entries
        if let Ok(entries) = fs::read_dir(directory) {
            let mut dirs: Vec<BrowserEntry> = Vec::new();
            let mut files: Vec<BrowserEntry> = Vec::new();

            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                // Skip hidden files/directories
                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(BrowserEntry {
                        path,
                        name,
                        entry_type: BrowserEntryType::Directory,
                        subsongs: 1,
                        sid_tooltip: None,
                    });
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
                            // For SID files, parse header for subsong count and tooltip metadata
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
                                        // Build tooltip: Title, Author, © Released, PAL | 1xSID | N tunes
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
                                    // Fallback: just get subsong count quickly
                                    None => (sid_info::quick_subsong_count(&path), None),
                                }
                            } else {
                                (1, None)
                            };

                            files.push(BrowserEntry {
                                path,
                                name,
                                entry_type: BrowserEntryType::MusicFile(ft),
                                subsongs,
                                sid_tooltip,
                            });
                        }
                    }
                }
            }

            // Sort directories and files alphabetically
            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

            // Directories first, then files
            self.browser_entries.extend(dirs);
            self.browser_entries.extend(files);
        }

        log::info!(
            "Loaded {} entries from {}",
            self.browser_entries.len(),
            directory.display()
        );
    }

    fn next_track(&mut self) {
        if self.playlist.is_empty() {
            self.current_playing = None;
            return;
        }

        if self.shuffle_enabled && !self.shuffle_order.is_empty() {
            if let Some(current) = self.current_playing {
                if let Some(pos) = self.shuffle_order.iter().position(|&x| x == current) {
                    let next_pos = pos + 1;
                    if next_pos < self.shuffle_order.len() {
                        self.current_playing = Some(self.shuffle_order[next_pos]);
                    } else {
                        self.current_playing = None;
                    }
                } else {
                    self.current_playing = Some(self.shuffle_order[0]);
                }
            } else {
                self.current_playing = Some(self.shuffle_order[0]);
            }
        } else {
            self.current_playing = self
                .current_playing
                .map(|i| {
                    let next = i + 1;
                    if next < self.playlist.len() {
                        Some(next)
                    } else {
                        None
                    }
                })
                .unwrap_or(Some(0));
        }
    }

    fn previous_track(&mut self) {
        if self.playlist.is_empty() {
            self.current_playing = None;
            return;
        }

        if self.shuffle_enabled && !self.shuffle_order.is_empty() {
            if let Some(current) = self.current_playing {
                if let Some(pos) = self.shuffle_order.iter().position(|&x| x == current) {
                    let prev_pos = if pos == 0 {
                        self.shuffle_order.len() - 1
                    } else {
                        pos - 1
                    };
                    self.current_playing = Some(self.shuffle_order[prev_pos]);
                } else {
                    self.current_playing = Some(self.shuffle_order[0]);
                }
            } else {
                self.current_playing = Some(self.shuffle_order[0]);
            }
        } else {
            self.current_playing = Some(
                self.current_playing
                    .map(|i| {
                        if i == 0 {
                            self.playlist.len() - 1
                        } else {
                            i - 1
                        }
                    })
                    .unwrap_or(0),
            );
        }
    }

    fn generate_shuffle_order(&mut self) {
        self.shuffle_order = (0..self.playlist.len()).collect();
        let mut rng = rand::thread_rng();
        self.shuffle_order.shuffle(&mut rng);
    }

    /// Set the default song duration (called from main.rs with settings value)
    pub fn set_default_song_duration(&mut self, duration: u32) {
        self.default_song_duration = duration;
    }

    fn get_song_duration(&self, entry: &PlaylistEntry) -> u32 {
        // Try to look up in song length database by current subsong
        // Note: subsong is 1-based, array is 0-based
        if let Some(hash) = &entry.md5_hash {
            if let Some(lengths) = self.song_lengths.get(hash) {
                let subsong_idx = (self.current_subsong as usize).saturating_sub(1);
                if subsong_idx < lengths.len() {
                    return lengths[subsong_idx];
                }
            }
        }

        // Fall back to stored duration (for subsong 1)
        if self.current_subsong == 1 {
            if let Some(dur) = entry.duration {
                return dur;
            }
        }

        // Default duration from settings
        self.default_song_duration
    }
}

// === Async Functions ===

async fn play_music_file(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    song_number: Option<u8>,
    file_type: MusicFileType,
) -> Result<(), String> {
    music_ops::play_music_file(connection, path, song_number, file_type).await
}

async fn download_song_lengths_async() -> Result<String, String> {
    music_ops::download_song_lengths_async().await
}

async fn parse_song_lengths_async(
    path: PathBuf,
) -> Result<HashMap<[u8; MD5_HASH_SIZE], Vec<u32>>, String> {
    music_ops::parse_song_lengths_async(path).await
}

async fn save_playlist_async(playlist: SavedPlaylist) -> Result<String, String> {
    music_ops::save_playlist_async(playlist).await
}

async fn load_playlist_async() -> Result<Vec<PlaylistEntry>, String> {
    music_ops::load_playlist_async().await
}

fn search_files_recursive(root: &Path, query: &str) -> Vec<BrowserEntry> {
    music_ops::search_files_recursive(root, query)
}

// === Helper Functions ===

fn truncate_string(s: &str, max_len: usize) -> String {
    crate::string_utils::truncate_string(s, max_len)
}

fn truncate_path(path: &Path, max_len: usize) -> String {
    crate::string_utils::truncate_path(path, max_len)
}

fn format_total_duration(entries: &[PlaylistEntry], default_duration: u32) -> String {
    music_ops::format_total_duration(entries, default_duration)
}
