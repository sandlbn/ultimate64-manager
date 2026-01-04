use iced::{
    Command, Element, Length,
    widget::{Column, button, column, row, scrollable, text},
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
    pub song_count: Option<u8>,
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
                self.current_song_number = 1;
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
        // Transport controls
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
            })
            .padding([8, 12]),
            button(text("Stop"))
                .on_press(MusicPlayerMessage::Stop)
                .padding([8, 12]),
            button(text("Prev"))
                .on_press(MusicPlayerMessage::Previous)
                .padding([8, 12]),
            button(text("Next"))
                .on_press(MusicPlayerMessage::Next)
                .padding([8, 12]),
        ]
        .spacing(5);

        // Mode toggles
        let mode_controls = row![
            button(text(if self.shuffle_enabled {
                "Shuffle: ON"
            } else {
                "Shuffle: OFF"
            }))
            .on_press(MusicPlayerMessage::ToggleShuffle)
            .padding([6, 10]),
            button(text(if self.repeat_enabled {
                "Repeat: ON"
            } else {
                "Repeat: OFF"
            }))
            .on_press(MusicPlayerMessage::ToggleRepeat)
            .padding([6, 10]),
        ]
        .spacing(5);

        // Now playing
        let now_playing = if let Some(index) = self.current_index {
            if let Some(file) = self.playlist.get(index) {
                let icon = match file.file_type {
                    MusicFileType::Sid => "[SID]",
                    MusicFileType::Mod => "[MOD]",
                };
                text(format!("{} Now Playing: {}", icon, file.name)).size(16)
            } else {
                text("No track selected").size(16)
            }
        } else {
            text("No track selected").size(16)
        };

        // Time display
        let time_display = text(format!(
            "{}:{:02}",
            self.playback_duration.as_secs() / 60,
            self.playback_duration.as_secs() % 60
        ))
        .size(14);

        // Song selector for SID files with multiple songs
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
                text("Song:").size(12),
                button(text("-").size(12))
                    .on_press(MusicPlayerMessage::SetSongNumber(
                        self.current_song_number.saturating_sub(1).max(1)
                    ))
                    .padding([4, 8]),
                text(format!("{}/{}", self.current_song_number, max_songs)).size(12),
                button(text("+").size(12))
                    .on_press(MusicPlayerMessage::SetSongNumber(
                        (self.current_song_number + 1).min(max_songs)
                    ))
                    .padding([4, 8]),
            ]
            .spacing(5)
            .align_items(iced::Alignment::Center)
            .into()
        } else {
            row![].into()
        };

        // Playlist header
        let playlist_header = row![
            text(format!("Playlist ({} files)", self.playlist.len())).size(14),
            button(text("Browse").size(12))
                .on_press(MusicPlayerMessage::SelectDirectory)
                .padding([4, 8]),
            button(text("Refresh").size(12))
                .on_press(MusicPlayerMessage::RefreshPlaylist)
                .padding([4, 8]),
        ]
        .spacing(10)
        .align_items(iced::Alignment::Center);

        // Playlist items
        let playlist_items: Vec<Element<'_, MusicPlayerMessage>> = self
            .playlist
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let icon = match file.file_type {
                    MusicFileType::Sid => "[SID]",
                    MusicFileType::Mod => "[MOD]",
                };

                let is_current = Some(index) == self.current_index;
                let prefix = if is_current {
                    match self.playback_state {
                        PlaybackState::Playing => ">",
                        PlaybackState::Paused => "=",
                        PlaybackState::Stopped => "*",
                    }
                } else {
                    " "
                };

                button(text(format!("{} {} {}", prefix, icon, file.name)).size(12))
                    .on_press(MusicPlayerMessage::FileSelected(index))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .into()
            })
            .collect();

        let playlist_scroll =
            scrollable(Column::with_children(playlist_items).spacing(2).padding(5))
                .height(Length::Fill);

        // Current directory
        let dir_display =
            text(format!("Dir: {}", self.current_directory.to_string_lossy())).size(11);

        column![
            text("MUSIC PLAYER").size(20),
            iced::widget::horizontal_rule(1),
            iced::widget::Space::with_height(10),
            now_playing,
            time_display,
            iced::widget::Space::with_height(10),
            controls,
            mode_controls,
            song_selector,
            iced::widget::Space::with_height(15),
            playlist_header,
            dir_display,
            playlist_scroll,
        ]
        .spacing(8)
        .padding(15)
        .into()
    }

    fn load_music_files(&mut self, directory: &Path) {
        self.playlist.clear();

        let walker = WalkDir::new(directory)
            .follow_links(true)
            .max_depth(5) // Limit recursion depth
            .into_iter()
            .filter_map(|entry| entry.ok());

        let mut files: Vec<MusicFile> = Vec::new();

        for entry in walker {
            let path = entry.path();

            if path.is_dir() {
                continue;
            }

            if let Some(extension) = path.extension() {
                if let Some(ext_str) = extension.to_str() {
                    let ext_lower = ext_str.to_lowercase();

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
                                song_count: Some(1), // TODO: Parse SID header
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

        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        log::info!(
            "Found {} music files in {}",
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

        self.current_song_number = 1;
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

        self.current_song_number = 1;
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
    log::info!("Playing: {} (song: {:?})", path.display(), song_number);

    let data = std::fs::read(&path).map_err(|e| e.to_string())?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase());

    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    tokio::task::spawn_blocking(move || {
        let conn = connection.blocking_lock();
        match ext.as_deref() {
            Some("sid") => conn.sid_play(&data, song_number).map_err(|e| e.to_string()),
            Some("mod") | Some("s3m") | Some("xm") | Some("it") => {
                conn.mod_play(&data).map_err(|e| e.to_string())
            }
            _ => Err("Unsupported music file type".to_string()),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}
