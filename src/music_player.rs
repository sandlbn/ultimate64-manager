use crate::mod_info;
use iced::{
    Task, Element, Length, Subscription,
    widget::{
        Column, Space, button, column, container, progress_bar, row, scrollable,
        text, text_input, tooltip, rule,
    },
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

// MD5 hash size
const MD5_HASH_SIZE: usize = 16;
const DEFAULT_SONG_DURATION: u32 = 180; // 3 minutes default
/// Timeout for REST API operations to prevent hangs when device goes offline
const REST_TIMEOUT_SECS: u64 = 5;

/// SID file header information
#[derive(Debug, Clone, Default)]
pub struct SidInfo {
    pub title: String,
    pub author: String,
    #[allow(dead_code)]
    pub released: String,
    pub songs: u8,
    #[allow(dead_code)]
    pub start_song: u8,
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
}

#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub name: String,
    pub entry_type: BrowserEntryType,
    pub subsongs: u8, // Number of subsongs (1 for non-SID or single-song SID)
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
                            if let Some(hash) = hex_to_md5(md5_str) {
                                let mut lengths = Vec::new();
                                for token in lengths_str.split_whitespace() {
                                    if let Some(duration) = parse_time_string(token) {
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
                self.load_browser_entries(&path);
                Task::none()
            }

            MusicPlayerMessage::NavigateToDirectory(path) => {
                self.browser_directory = path.clone();
                self.browser_selected = None;
                self.load_browser_entries(&path);
                Task::none()
            }

            MusicPlayerMessage::NavigateUp => {
                if let Some(parent) = self.browser_directory.parent() {
                    let parent = parent.to_path_buf();
                    self.browser_directory = parent.clone();
                    self.browser_selected = None;
                    self.load_browser_entries(&parent);
                }
                Task::none()
            }

            MusicPlayerMessage::RefreshBrowser => {
                self.browser_selected = None;
                self.load_browser_entries(&self.browser_directory.clone());
                Task::none()
            }

            MusicPlayerMessage::BrowserFilterChanged(value) => {
                self.browser_filter = value;
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
                                    let hash = compute_md5(&data);
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
                    .map(|entry| entry.file_type == MusicFileType::Mod)
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
        let now_playing_text = if let Some(idx) = self.current_playing {
            if let Some(entry) = self.playlist.get(idx) {
                let icon = match entry.file_type {
                    MusicFileType::Sid => "[SID]",
                    MusicFileType::Mod => "[MOD]",
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
                format!("{} {}", icon, name)
            } else {
                "No track selected".to_string()
            }
        } else {
            "No track selected".to_string()
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
                        "Pause"
                    } else {
                        "Play"
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
                button(text("Stop").size(normal))
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
                        "Shuffle: ON"
                    } else {
                        "Shuffle"
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
                        "Repeat: ON"
                    } else {
                        "Repeat"
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

        let top_bar = column![
            row![now_playing, Space::new().width(Length::Fill), time_display]
                .align_y(iced::Alignment::Center),
            row![transport, Space::new().width(20), modes].align_y(iced::Alignment::Center),
            // Progress bar
            container(
                progress_bar(
                    0.0..=self.current_song_duration as f32,
                    self.elapsed_seconds as f32
                )
            )
            .width(Length::Fill)
            .height(Length::Fixed(8.0))
            .padding([5, 0]),
        ]
        .spacing(8)
        .padding(10);

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
                text("LOCAL FILES").size(normal),
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
                        "Add all music files from current directory to playlist",
                        tooltip::Position::Bottom,
                    )
                    .style(container::bordered_box),
                    Space::new().width(Length::Fill),
                    text("Filter:").size(small),
                    text_input("filter...", &self.browser_filter)
                        .on_input(MusicPlayerMessage::BrowserFilterChanged)
                        .size(small)
                        .padding(4)
                        .width(Length::Fixed(100.0)),
                ]
                .spacing(5)
                .align_y(iced::Alignment::Center),
                text(dir_display.clone()).size(small),
                text(format!("{} music files", music_file_count)).size(small),
            ]
            .spacing(5),
        )
        .padding(10);

        let browser_list: Element<'_, MusicPlayerMessage> = if self.browser_entries.is_empty() {
            container(text("Empty directory").size(normal))
                .padding(10)
                .into()
        } else {
            // Filter entries based on filter text
            let filtered_entries: Vec<(usize, &BrowserEntry)> = self
                .browser_entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    self.browser_filter.is_empty()
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
                            row![
                                button(text(format!("[DIR] {}", entry.name)).size(normal))
                                    .on_press(MusicPlayerMessage::BrowserItemClicked(*idx))
                                    .padding([6, 8])
                                    .width(Length::Fill)
                                    .style(button::text),
                            ]
                            .into()
                        }
                        BrowserEntryType::MusicFile(ft) => {
                            // Music file entry - show add and play buttons
                            let icon = match ft {
                                MusicFileType::Sid => "[SID]",
                                MusicFileType::Mod => "[MOD]",
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

                            let file_element: Element<'_, MusicPlayerMessage> = if is_truncated {
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

                    let icon = match entry.file_type {
                        MusicFileType::Sid => "S",
                        MusicFileType::Mod => "M",
                    };

                    // Show subsong count for multi-subsong files
                    let subsong_info = if entry.max_subsongs > 1 {
                        format!("x{}", entry.max_subsongs)
                    } else {
                        String::new()
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
                            "{} [{}{}] {} ({})",
                            prefix, icon, subsong_info, name, duration_str
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

                    let playlist_element: Element<'_, MusicPlayerMessage> = if is_truncated {
                        tooltip(
                            playlist_button,
                            text(display_name.clone()).size(small),
                            tooltip::Position::Top,
                        )
                        .style(container::bordered_box)
                        .into()
                    } else {
                        playlist_button.into()
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
            row![
                tooltip(
                    button(text("Play Selected").size(small))
                        .on_press(MusicPlayerMessage::PlaylistItemDoubleClick(selected))
                        .padding([4, 10]),
                    "Start playing the selected track",
                    tooltip::Position::Bottom,
                )
                .style(container::bordered_box),
            ]
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
        let main_content = row![browser_pane, rule::vertical(1), playlist_pane].height(Length::Fill);

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
        };

        // Parse SID header only when adding to playlist (lazy loading)
        if entry.file_type == MusicFileType::Sid {
            if let Ok(data) = fs::read(&entry.path) {
                // Parse SID header for display name
                if let Some(info) = parse_sid_header(&data) {
                    entry.name = if info.title.is_empty() {
                        browser_entry.name.clone()
                    } else if info.author.is_empty() {
                        info.title
                    } else {
                        format!("{} - {}", info.author, info.title)
                    };
                    // Use SID header subsong count as baseline
                    entry.max_subsongs = info.songs;
                } else {
                    entry.name = browser_entry.name.clone();
                }

                // Calculate MD5 and look up in song length database
                let hash = compute_md5(&data);
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
                    });
                } else if let Some(extension) = path.extension() {
                    if let Some(ext_str) = extension.to_str() {
                        let ext_lower = ext_str.to_lowercase();
                        let file_type = match ext_lower.as_str() {
                            "sid" => Some(MusicFileType::Sid),
                            "mod" => Some(MusicFileType::Mod),
                            _ => None,
                        };

                        if let Some(ft) = file_type {
                            // Parse SID file to get subsong count
                            let subsongs = if ft == MusicFileType::Sid {
                                parse_sid_subsong_count(&path)
                            } else {
                                1
                            };

                            files.push(BrowserEntry {
                                path,
                                name,
                                entry_type: BrowserEntryType::MusicFile(ft),
                                subsongs,
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

// === SID Header Parsing ===

/// Quick parse to get just subsong count (for browser display)
fn parse_sid_subsong_count(path: &Path) -> u8 {
    // Only read first 16 bytes needed for subsong count
    if let Ok(file) = fs::File::open(path) {
        use std::io::Read;
        let mut buffer = [0u8; 16];
        let mut reader = std::io::BufReader::new(file);
        if reader.read_exact(&mut buffer).is_ok() {
            // Check PSID or RSID magic
            if &buffer[0..4] == b"PSID" || &buffer[0..4] == b"RSID" {
                // Subsong count at offset 0x0E (big-endian)
                let songs = ((buffer[14] as u16) << 8) | (buffer[15] as u16);
                return if songs > 0 { songs as u8 } else { 1 };
            }
        }
    }
    1 // Default to 1 subsong
}

fn parse_sid_header(data: &[u8]) -> Option<SidInfo> {
    if data.len() < 0x76 {
        return None;
    }

    let magic = &data[0..4];
    if magic != b"PSID" && magic != b"RSID" {
        return None;
    }

    let songs = u16::from_be_bytes([data[0x0E], data[0x0F]]) as u8;
    let start_song = u16::from_be_bytes([data[0x10], data[0x11]]) as u8;
    let title = read_sid_string(&data[0x16..0x36]);
    let author = read_sid_string(&data[0x36..0x56]);
    let released = read_sid_string(&data[0x56..0x76]);

    Some(SidInfo {
        title,
        author,
        released,
        songs: songs.max(1),
        start_song: start_song.max(1),
    })
}

fn read_sid_string(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    data[..end]
        .iter()
        .filter_map(|&b| {
            if b >= 32 && b < 127 {
                Some(b as char)
            } else {
                None
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

// === Async Functions ===

async fn play_music_file(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    song_number: Option<u8>,
    file_type: MusicFileType,
) -> Result<(), String> {
    log::info!("Playing: {} (song: {:?})", path.display(), song_number);

    let data = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            match file_type {
                MusicFileType::Sid => conn.sid_play(&data, song_number).map_err(|e| e.to_string()),
                MusicFileType::Mod => conn.mod_play(&data).map_err(|e| e.to_string()),
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

async fn download_song_lengths_async() -> Result<String, String> {
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

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

async fn parse_song_lengths_async(
    path: PathBuf,
) -> Result<HashMap<[u8; MD5_HASH_SIZE], Vec<u32>>, String> {
    // Song length database format:
    // Each line: MD5HASH=duration1 duration2 duration3 ...
    // Where each duration corresponds to a subsong (subsong 1 = index 0, etc.)
    // The number of durations = number of subsongs in the file
    // This is more authoritative than the SID header subsong count

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Cannot read file: {}", e))?;

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

            if let Some(hash) = hex_to_md5(md5_str) {
                let mut lengths = Vec::new();

                for token in lengths_str.split_whitespace() {
                    if let Some(duration) = parse_time_string(token) {
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

async fn save_playlist_async(playlist: SavedPlaylist) -> Result<String, String> {
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

async fn load_playlist_async() -> Result<Vec<PlaylistEntry>, String> {
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

// === Helper Functions ===

fn compute_md5(data: &[u8]) -> [u8; MD5_HASH_SIZE] {
    let digest = md5::compute(data);
    digest.0
}

fn hex_to_md5(hex_str: &str) -> Option<[u8; MD5_HASH_SIZE]> {
    if hex_str.len() != 32 {
        return None;
    }

    let mut result = [0u8; MD5_HASH_SIZE];

    for (i, byte) in result.iter_mut().enumerate() {
        let hex_byte = &hex_str[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(hex_byte, 16).ok()?;
    }

    Some(result)
}

fn parse_time_string(s: &str) -> Option<u32> {
    // Handle formats: "M:SS", "M:SS.mmm", "H:MM:SS", "H:MM:SS.mmm", or just seconds
    // Strip any milliseconds (after the decimal point)
    let s = s.split('.').next().unwrap_or(s);

    let parts: Vec<&str> = s.split(':').collect();

    match parts.len() {
        1 => parts[0].parse().ok(),
        2 => {
            let minutes: u32 = parts[0].parse().ok()?;
            let seconds: u32 = parts[1].parse().ok()?;
            Some(minutes * 60 + seconds)
        }
        3 => {
            let hours: u32 = parts[0].parse().ok()?;
            let minutes: u32 = parts[1].parse().ok()?;
            let seconds: u32 = parts[2].parse().ok()?;
            Some(hours * 3600 + minutes * 60 + seconds)
        }
        _ => None,
    }
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

fn truncate_path(path: &Path, max_len: usize) -> String {
    let s = path.to_string_lossy();
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("...{}", &s[s.len().saturating_sub(max_len - 3)..])
    }
}

fn format_total_duration(entries: &[PlaylistEntry], default_duration: u32) -> String {
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