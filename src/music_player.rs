use iced::{
    widget::{button, column, row, scrollable, text, Column},
    Command, Element, Length,
};
use rand::seq::SliceRandom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use ultimate64::Rest;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub enum MusicPlayerMessage {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    ToggleShuffle,
    ToggleRepeat,
    SelectDirectory,
    DirectorySelected(PathBuf),
    FileSelected(usize),
    PlayFile(PathBuf, Option<u8>),
    PlaybackCompleted(Result<(), String>),
    UpdatePlaybackTime,
    SetSongNumber(u8),
    RefreshPlaylist,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone)]
pub struct MusicFile {
    pub path: PathBuf,
    pub name: String,
    pub file_type: MusicFileType,
    pub song_count: Option<u8>, // For SID files with multiple songs
}

#[derive(Debug, Clone, PartialEq)]
pub enum MusicFileType {
    Sid,
    Mod,
}

pub struct MusicPlayer {
    playlist: Vec<MusicFile>,
    current_index: Option<usize>,
    playback_state: PlaybackState,
    shuffle_enabled: bool,
    repeat_enabled: bool,
    current_directory: PathBuf,
    current_song_number: u8,
    playback_duration: Duration,
    shuffle_order: Vec<usize>,
}

impl MusicPlayer {
    pub fn new() -> Self {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            playlist: Vec::new(),
            current_index: None,
            playback_state: PlaybackState::Stopped,
            shuffle_enabled: false,
            repeat_enabled: false,
            current_directory: home_dir,
            current_song_number: 1,
            playback_duration: Duration::from_secs(0),
            shuffle_order: Vec::new(),
        }
    }

    pub fn update(
        &mut self,
        message: MusicPlayerMessage,
        connection: Option<Arc<Mutex<Rest>>>,
    ) -> Command<MusicPlayerMessage> {
        match message {
            MusicPlayerMessage::Play => {
                if let Some(index) = self.current_index {
                    if let Some(file) = self.playlist.get(index) {
                        self.playback_state = PlaybackState::Playing;
                        if let Some(conn) = connection {
                            return Command::perform(
                                play_music_file(
                                    conn,
                                    file.path.clone(),
                                    Some(self.current_song_number),
                                ),
                                MusicPlayerMessage::PlaybackCompleted,
                            );
                        }
                    }
                } else if !self.playlist.is_empty() {
                    self.current_index = Some(0);
                    return self.update(MusicPlayerMessage::Play, connection);
                }
                Command::none()
            }
            MusicPlayerMessage::Pause => {
                self.playback_state = PlaybackState::Paused;
                // Note: Ultimate64 doesn't support pause, so we just stop
                Command::none()
            }
            MusicPlayerMessage::Stop => {
                self.playback_state = PlaybackState::Stopped;
                self.playback_duration = Duration::from_secs(0);
                Command::none()
            }
            MusicPlayerMessage::Next => {
                self.next_track();
                if self.playback_state == PlaybackState::Playing {
                    self.update(MusicPlayerMessage::Play, connection)
                } else {
                    Command::none()
                }
            }
            MusicPlayerMessage::Previous => {
                self.previous_track();
                if self.playback_state == PlaybackState::Playing {
                    self.update(MusicPlayerMessage::Play, connection)
                } else {
                    Command::none()
                }
            }
            MusicPlayerMessage::ToggleShuffle => {
                self.shuffle_enabled = !self.shuffle_enabled;
                if self.shuffle_enabled {
                    self.generate_shuffle_order();
                }
                Command::none()
            }
            MusicPlayerMessage::ToggleRepeat => {
                self.repeat_enabled = !self.repeat_enabled;
                Command::none()
            }
            MusicPlayerMessage::SelectDirectory => Command::perform(
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
                        MusicPlayerMessage::RefreshPlaylist
                    }
                },
            ),
            MusicPlayerMessage::DirectorySelected(path) => {
                self.current_directory = path.clone();
                self.load_music_files(&path);
                Command::none()
            }
            MusicPlayerMessage::FileSelected(index) => {
                self.current_index = Some(index);
                if self.playback_state == PlaybackState::Playing {
                    self.update(MusicPlayerMessage::Play, connection)
                } else {
                    Command::none()
                }
            }
            MusicPlayerMessage::PlayFile(path, song_num) => {
                if let Some(conn) = connection {
                    Command::perform(
                        play_music_file(conn, path, song_num),
                        MusicPlayerMessage::PlaybackCompleted,
                    )
                } else {
                    Command::none()
                }
            }
            MusicPlayerMessage::PlaybackCompleted(result) => {
                if let Err(e) = result {
                    log::error!("Playback failed: {}", e);
                }
                // Auto-advance to next track if repeat is disabled
                if !self.repeat_enabled && self.playback_state == PlaybackState::Playing {
                    self.next_track();
                    return self.update(MusicPlayerMessage::Play, connection);
                } else if self.repeat_enabled && self.playback_state == PlaybackState::Playing {
                    // Repeat current track
                    return self.update(MusicPlayerMessage::Play, connection);
                }
                Command::none()
            }
            MusicPlayerMessage::UpdatePlaybackTime => {
                if self.playback_state == PlaybackState::Playing {
                    self.playback_duration += Duration::from_secs(1);
                }
                Command::none()
            }
            MusicPlayerMessage::SetSongNumber(num) => {
                self.current_song_number = num;
                if self.playback_state == PlaybackState::Playing {
                    self.update(MusicPlayerMessage::Play, connection)
                } else {
                    Command::none()
                }
            }
            MusicPlayerMessage::RefreshPlaylist => {
                self.load_music_files(&self.current_directory.clone());
                Command::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, MusicPlayerMessage> {
        let controls = row![
            button(text(if self.playback_state == PlaybackState::Playing {
                "Pause"
            } else {
                "Play"
            }))
            .on_press(if self.playback_state == PlaybackState::Playing {
                MusicPlayerMessage::Pause
            } else {
                MusicPlayerMessage::Play
            }),
            button(text("Stop")).on_press(MusicPlayerMessage::Stop),
            button(text("Previous")).on_press(MusicPlayerMessage::Previous),
            button(text("Next")).on_press(MusicPlayerMessage::Next),
            button(text(if self.shuffle_enabled {
                "Shuffle: ON"
            } else {
                "Shuffle: OFF"
            }))
            .on_press(MusicPlayerMessage::ToggleShuffle),
            button(text(if self.repeat_enabled {
                "Repeat: ON"
            } else {
                "Repeat: OFF"
            }))
            .on_press(MusicPlayerMessage::ToggleRepeat),
        ]
        .spacing(10);

        let current_track = if let Some(index) = self.current_index {
            if let Some(file) = self.playlist.get(index) {
                text(format!("Now Playing: {}", file.name)).size(18)
            } else {
                text("No track selected").size(18)
            }
        } else {
            text("No track selected").size(18)
        };

        let time_display = text(format!(
            "Time: {}:{:02}",
            self.playback_duration.as_secs() / 60,
            self.playback_duration.as_secs() % 60
        ));

        let song_selector: Element<'_, MusicPlayerMessage> = if self
            .current_index
            .and_then(|i| self.playlist.get(i))
            .and_then(|f| f.song_count)
            .is_some()
        {
            let max_songs = self.playlist[self.current_index.unwrap()]
                .song_count
                .unwrap();
            row![
                text("Song #:"),
                button(text("-")).on_press(MusicPlayerMessage::SetSongNumber(
                    self.current_song_number.saturating_sub(1).max(1)
                )),
                text(format!("{}/{}", self.current_song_number, max_songs)),
                button(text("+")).on_press(MusicPlayerMessage::SetSongNumber(
                    (self.current_song_number + 1).min(max_songs)
                )),
            ]
            .spacing(5)
            .into()
        } else {
            row![].into()
        };

        let playlist_header = row![
            text(format!("Playlist ({} files)", self.playlist.len())).size(16),
            button(text("Browse Folder")).on_press(MusicPlayerMessage::SelectDirectory),
            button(text("Refresh")).on_press(MusicPlayerMessage::RefreshPlaylist),
        ]
        .spacing(10);

        let playlist_items: Vec<Element<'_, MusicPlayerMessage>> = self
            .playlist
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let type_indicator = match file.file_type {
                    MusicFileType::Sid => "[SID]",
                    MusicFileType::Mod => "[MOD]",
                };

                let is_current = Some(index) == self.current_index;
                let playing_indicator = if is_current {
                    if self.playback_state == PlaybackState::Playing {
                        "> "
                    } else {
                        "* "
                    }
                } else {
                    "  "
                };

                let track_button = button(text(format!(
                    "{}{} {}",
                    playing_indicator, type_indicator, file.name
                )))
                .on_press(MusicPlayerMessage::FileSelected(index))
                .width(Length::Fill);

                row![track_button].into()
            })
            .collect();

        let playlist_scroll =
            scrollable(Column::with_children(playlist_items).spacing(2).padding(5))
                .height(Length::FillPortion(3));

        column![
            current_track,
            controls,
            time_display,
            song_selector,
            playlist_header,
            playlist_scroll,
        ]
        .spacing(15)
        .padding(20)
        .into()
    }

    fn load_music_files(&mut self, directory: &Path) {
        self.playlist.clear();

        // Use WalkDir to recursively scan all subdirectories
        let walker = WalkDir::new(directory)
            .follow_links(true)
            .into_iter()
            .filter_map(|entry| entry.ok());

        let mut files: Vec<MusicFile> = Vec::new();

        for entry in walker {
            let path = entry.path();

            // Skip directories
            if path.is_dir() {
                continue;
            }

            // Get file extension and check if it's a music file
            if let Some(extension) = path.extension() {
                if let Some(ext_str) = extension.to_str() {
                    let ext_lower = ext_str.to_lowercase();

                    // Get relative path for display
                    let display_name = if let Ok(relative) = path.strip_prefix(directory) {
                        relative.to_string_lossy().to_string()
                    } else {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    };

                    match ext_lower.as_str() {
                        "sid" => {
                            files.push(MusicFile {
                                path: path.to_path_buf(),
                                name: display_name,
                                file_type: MusicFileType::Sid,
                                song_count: Some(1), // Would need to parse SID header for actual count
                            });
                        }
                        "mod" | "s3m" | "xm" | "it" => {
                            files.push(MusicFile {
                                path: path.to_path_buf(),
                                name: display_name,
                                file_type: MusicFileType::Mod,
                                song_count: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }

        // Sort files by path/name
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        log::info!(
            "Found {} music files in {} (including subdirectories)",
            files.len(),
            directory.display()
        );

        self.playlist = files;

        if self.shuffle_enabled {
            self.generate_shuffle_order();
        }
    }

    fn next_track(&mut self) {
        if self.playlist.is_empty() {
            return;
        }

        if self.shuffle_enabled && !self.shuffle_order.is_empty() {
            // Find current position in shuffle order and move to next
            if let Some(current) = self.current_index {
                if let Some(pos) = self.shuffle_order.iter().position(|&x| x == current) {
                    let next_pos = (pos + 1) % self.shuffle_order.len();
                    self.current_index = Some(self.shuffle_order[next_pos]);
                } else {
                    self.current_index = Some(self.shuffle_order[0]);
                }
            } else {
                self.current_index = Some(self.shuffle_order[0]);
            }
        } else {
            self.current_index = Some(
                self.current_index
                    .map(|i| (i + 1) % self.playlist.len())
                    .unwrap_or(0),
            );
        }

        self.current_song_number = 1; // Reset to first song
    }

    fn previous_track(&mut self) {
        if self.playlist.is_empty() {
            return;
        }

        if self.shuffle_enabled && !self.shuffle_order.is_empty() {
            if let Some(current) = self.current_index {
                if let Some(pos) = self.shuffle_order.iter().position(|&x| x == current) {
                    let prev_pos = if pos == 0 {
                        self.shuffle_order.len() - 1
                    } else {
                        pos - 1
                    };
                    self.current_index = Some(self.shuffle_order[prev_pos]);
                } else {
                    self.current_index = Some(self.shuffle_order[0]);
                }
            } else {
                self.current_index = Some(self.shuffle_order[0]);
            }
        } else {
            self.current_index = Some(
                self.current_index
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

        self.current_song_number = 1; // Reset to first song
    }

    fn generate_shuffle_order(&mut self) {
        self.shuffle_order = (0..self.playlist.len()).collect();
        let mut rng = rand::thread_rng();
        self.shuffle_order.shuffle(&mut rng);
    }
}

async fn play_music_file(
    connection: Arc<Mutex<Rest>>,
    path: PathBuf,
    song_number: Option<u8>,
) -> Result<(), String> {
    let conn = connection.lock().await;
    let data = std::fs::read(&path).map_err(|e| e.to_string())?;

    match path.extension().and_then(|s| s.to_str()) {
        Some("sid") => conn.sid_play(&data, song_number).map_err(|e| e.to_string()),
        Some("mod") => conn.mod_play(&data).map_err(|e| e.to_string()),
        _ => Err("Unsupported music file type".to_string()),
    }
}
