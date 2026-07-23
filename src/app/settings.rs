//! Settings, profile CRUD, device discovery, templates, and directory
//! pickers. Extracted from `main.rs::update`.

use iced::Task;
use std::path::PathBuf;

use crate::discovery::{self, DiscoveredDevice};
use crate::execute_template_commands;
use crate::settings::ConnectionSettings;
use crate::templates::DiskTemplate;
use crate::{Message, Ultimate64Browser, UserMessage};

impl Ultimate64Browser {
    pub(crate) fn handle_save_profile(&mut self) -> Task<Message> {
        // Sync current input fields to the active profile before saving
        let conn_settings = ConnectionSettings {
            host: self.host_input.clone(),
            password: if self.password_input.is_empty() {
                None
            } else {
                Some(self.password_input.clone())
            },
            stream_control_method: self.settings.connection.stream_control_method,
        };
        self.profile_manager.active_settings_mut().connection = conn_settings;

        if let Ok(size) = self.font_size_input.parse::<u32>() {
            if size >= 8 && size <= 24 {
                self.profile_manager
                    .active_settings_mut()
                    .preferences
                    .font_size = size;
            }
        }

        self.settings = self.profile_manager.active_settings().clone();

        match self.profile_manager.save() {
            Ok(()) => {
                self.user_message =
                    Some(UserMessage::Info("Profile saved successfully".to_string()));
            }
            Err(e) => {
                self.user_message =
                    Some(UserMessage::Error(format!("Failed to save profile: {}", e)));
            }
        }
        Task::none()
    }

    pub(crate) fn handle_start_discovery(&mut self) -> Task<Message> {
        if self.is_discovering {
            return Task::none();
        }
        self.is_discovering = true;
        self.discovered_devices.clear();
        self.user_message = Some(UserMessage::Info("Scanning network...".to_string()));

        Task::perform(discovery::discover_devices(), Message::DiscoveryComplete)
    }

    pub(crate) fn handle_discovery_complete(
        &mut self,
        devices: Vec<DiscoveredDevice>,
    ) -> Task<Message> {
        self.is_discovering = false;
        self.discovered_devices = devices.clone();
        self.discovery_ran = true;

        if devices.is_empty() {
            self.user_message = Some(UserMessage::Info(
                "No Ultimate devices found on network".to_string(),
            ));
        } else {
            self.user_message = Some(UserMessage::Info(format!(
                "Found {} device(s)",
                devices.len()
            )));
        }
        Task::none()
    }

    pub(crate) fn handle_select_discovered_device(
        &mut self,
        device: DiscoveredDevice,
    ) -> Task<Message> {
        self.host_input = device.ip.clone();
        self.user_message = Some(UserMessage::Info(format!(
            "Selected: {} ({})",
            device.product, device.ip
        )));
        Task::none()
    }

    pub(crate) fn handle_profile_selected(&mut self, name: String) -> Task<Message> {
        if self.profile_manager.switch_profile(&name) {
            self.settings = self.profile_manager.active_settings().clone();
            self.host_input = self.settings.connection.host.clone();
            self.password_input = self
                .settings
                .connection
                .password
                .clone()
                .unwrap_or_default();
            self.font_size_input = self.settings.preferences.font_size.to_string();
            self.video_streaming
                .set_stream_control_method(self.settings.connection.stream_control_method);

            self.user_message = Some(UserMessage::Info(format!("Switched to profile: {}", name)));
            // Disconnect when switching profiles
            return Task::done(Message::DisconnectPressed);
        }
        Task::none()
    }

    pub(crate) fn handle_new_profile_name_changed(&mut self, name: String) -> Task<Message> {
        self.new_profile_name = name;
        Task::none()
    }

    pub(crate) fn handle_create_profile(&mut self) -> Task<Message> {
        let name = self.new_profile_name.trim().to_string();
        if name.is_empty() {
            self.user_message = Some(UserMessage::Error(
                "Profile name cannot be empty".to_string(),
            ));
        } else if self.profile_manager.add_profile(name.clone()) {
            self.new_profile_name.clear();

            self.user_message = Some(UserMessage::Info(format!("Created profile: {}", name)));
        } else {
            self.user_message = Some(UserMessage::Error(
                "Profile name already exists".to_string(),
            ));
        }
        Task::none()
    }

    pub(crate) fn handle_duplicate_profile(&mut self) -> Task<Message> {
        let new_name = format!("{} (copy)", self.profile_manager.active_profile);
        if self.profile_manager.duplicate_profile(
            &self.profile_manager.active_profile.clone(),
            new_name.clone(),
        ) {
            self.user_message = Some(UserMessage::Info(format!("Duplicated to: {}", new_name)));
        }
        Task::none()
    }

    pub(crate) fn handle_delete_profile(&mut self) -> Task<Message> {
        let name = self.profile_manager.active_profile.clone();
        if self.profile_manager.delete_profile(&name) {
            self.settings = self.profile_manager.active_settings().clone();

            self.user_message = Some(UserMessage::Info(format!("Deleted profile: {}", name)));
        } else {
            self.user_message = Some(UserMessage::Error(
                "Cannot delete active or last profile".to_string(),
            ));
        }
        Task::none()
    }

    pub(crate) fn handle_rename_profile_name_changed(&mut self, name: String) -> Task<Message> {
        self.rename_profile_name = name;
        Task::none()
    }

    pub(crate) fn handle_rename_profile(&mut self) -> Task<Message> {
        let new_name = self.rename_profile_name.trim().to_string();
        let old_name = self.profile_manager.active_profile.clone();
        if new_name.is_empty() {
            self.user_message = Some(UserMessage::Error(
                "Profile name cannot be empty".to_string(),
            ));
        } else if self
            .profile_manager
            .rename_profile(&old_name, new_name.clone())
        {
            self.rename_profile_name.clear();

            self.user_message = Some(UserMessage::Info(format!("Renamed to: {}", new_name)));
        } else {
            self.user_message = Some(UserMessage::Error(
                "Profile name already exists".to_string(),
            ));
        }
        Task::none()
    }

    pub(crate) fn handle_template_selected(&mut self, template: DiskTemplate) -> Task<Message> {
        self.selected_template = Some(template);
        Task::none()
    }

    pub(crate) fn handle_execute_template(&mut self) -> Task<Message> {
        if let Some(template) = &self.selected_template {
            if let Some(conn) = &self.connection {
                let conn = conn.clone();
                let commands = template.commands.clone();
                return Task::perform(
                    async move { execute_template_commands(conn, commands).await },
                    |result| match result {
                        Ok(_) => Message::RefreshStatus,
                        Err(e) => Message::ShowError(e),
                    },
                );
            } else {
                self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            }
        }
        Task::none()
    }

    pub(crate) fn handle_default_song_duration_changed(&mut self, value: String) -> Task<Message> {
        if let Ok(duration) = value.parse::<u32>() {
            if duration > 0 && duration <= 3600 {
                self.profile_manager
                    .active_settings_mut()
                    .preferences
                    .default_song_duration = duration;
                self.settings = self.profile_manager.active_settings().clone();
                self.music_player.set_default_song_duration(duration);
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
            }
        }
        Task::none()
    }

    pub(crate) fn handle_font_size_changed(&mut self, value: String) -> Task<Message> {
        self.font_size_input = value.clone();
        if let Ok(size) = value.parse::<u32>() {
            if size >= 8 && size <= 24 {
                self.profile_manager
                    .active_settings_mut()
                    .preferences
                    .font_size = size;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
            }
        }
        Task::none()
    }

    /// Settings: user is typing a new Game Mode library root path.
    pub(crate) fn handle_game_library_input_changed(&mut self, value: String) -> Task<Message> {
        self.game_library_input = value;
        Task::none()
    }

    /// Settings: add the staged path as a Game Mode library root (deduped),
    /// persist, and clear the input.
    pub(crate) fn handle_game_library_add_root(&mut self) -> Task<Message> {
        let root = self
            .game_library_input
            .trim()
            .trim_end_matches('/')
            .to_string();
        if root.is_empty() {
            return Task::none();
        }
        let root = if root.starts_with('/') {
            root
        } else {
            format!("/{}", root)
        };
        {
            let prefs = &mut self.profile_manager.active_settings_mut().preferences;
            if !prefs.game_library_roots.iter().any(|r| r == &root) {
                prefs.game_library_roots.push(root);
            }
        }
        self.settings = self.profile_manager.active_settings().clone();
        self.game_library_input.clear();
        if let Err(e) = self.profile_manager.save() {
            log::error!("Failed to save profiles: {}", e);
        }
        Task::none()
    }

    /// Settings: remove the library root at `idx`, persist.
    pub(crate) fn handle_game_library_remove_root(&mut self, idx: usize) -> Task<Message> {
        {
            let roots = &mut self
                .profile_manager
                .active_settings_mut()
                .preferences
                .game_library_roots;
            if idx < roots.len() {
                roots.remove(idx);
            }
        }
        self.settings = self.profile_manager.active_settings().clone();
        if let Err(e) = self.profile_manager.save() {
            log::error!("Failed to save profiles: {}", e);
        }
        Task::none()
    }

    pub(crate) fn handle_file_browser_start_dir_selected(
        &mut self,
        path: Option<PathBuf>,
    ) -> Task<Message> {
        if let Some(p) = path {
            self.profile_manager
                .active_settings_mut()
                .default_paths
                .file_browser_start_dir = Some(p);
            self.settings = self.profile_manager.active_settings().clone();

            self.user_message = Some(UserMessage::Info(
                "File Browser start directory set (restart app to apply)".to_string(),
            ));
        }
        Task::none()
    }

    pub(crate) fn handle_clear_file_browser_start_dir(&mut self) -> Task<Message> {
        self.profile_manager
            .active_settings_mut()
            .default_paths
            .file_browser_start_dir = None;
        self.settings = self.profile_manager.active_settings().clone();
        if let Err(e) = self.profile_manager.save() {
            log::error!("Failed to save profiles: {}", e);
        }
        self.user_message = Some(UserMessage::Info(
            "File Browser start directory cleared".to_string(),
        ));
        Task::none()
    }

    pub(crate) fn handle_music_player_start_dir_selected(
        &mut self,
        path: Option<PathBuf>,
    ) -> Task<Message> {
        if let Some(p) = path {
            self.profile_manager
                .active_settings_mut()
                .default_paths
                .music_player_start_dir = Some(p);
            self.settings = self.profile_manager.active_settings().clone();

            self.user_message = Some(UserMessage::Info(
                "Music Player start directory set (restart app to apply)".to_string(),
            ));
        }
        Task::none()
    }

    pub(crate) fn handle_clear_music_player_start_dir(&mut self) -> Task<Message> {
        self.profile_manager
            .active_settings_mut()
            .default_paths
            .music_player_start_dir = None;
        self.settings = self.profile_manager.active_settings().clone();
        if let Err(e) = self.profile_manager.save() {
            log::error!("Failed to save profiles: {}", e);
        }
        self.user_message = Some(UserMessage::Info(
            "Music Player start directory cleared".to_string(),
        ));
        Task::none()
    }
}
