//! Settings tab view. Extracted from .

use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, text, text_input, Space,
};
use iced::{Element, Length};

use crate::settings::StreamControlMethod;
use crate::{Message, Ultimate64Browser};

impl Ultimate64Browser {
    pub(crate) fn view_settings(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let dim = iced::Color::from_rgb(0.55, 0.55, 0.6);
        let header_color = iced::Color::from_rgb(0.7, 0.72, 0.8);

        macro_rules! section {
            ($title:expr, $content:expr) => {
                container(
                    column![
                        text($title).size(fs.large).color(header_color),
                        rule::horizontal(1),
                        Space::new().height(8),
                        $content,
                    ]
                    .spacing(4),
                )
                .padding(15)
                .width(Length::Fill)
                .style(crate::styles::section_style)
            };
        }

        // ── Profiles ─────────────────────────────────────────────────────
        let profile_names = self.profile_manager.profile_names();
        let profile_section = section!(
            "Profiles",
            column![
                row![
                    text("Active:").size(fs.normal).color(dim),
                    pick_list(
                        profile_names,
                        Some(self.profile_manager.active_profile.clone()),
                        Message::ProfileSelected,
                    )
                    .text_size(fs.small as f32)
                    .width(Length::Fixed(180.0)),
                    button(text("Save").size(fs.small))
                        .on_press(Message::SaveProfile)
                        .padding([4, 10])
                        .style(crate::styles::action_button),
                    button(text("Duplicate").size(fs.small))
                        .on_press(Message::DuplicateProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                    button(text("Delete").size(fs.small))
                        .on_press(Message::DeleteProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("New:").size(fs.normal).color(dim),
                    text_input("Profile name...", &self.new_profile_name)
                        .on_input(Message::NewProfileNameChanged)
                        .on_submit(Message::CreateProfile)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(180.0)),
                    button(text("Create").size(fs.small))
                        .on_press(Message::CreateProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Connection ───────────────────────────────────────────────────
        let discovery_button: Element<'_, Message> = if self.is_discovering {
            button(text("Scanning...").size(fs.small))
                .padding([4, 10])
                .style(crate::styles::nav_button)
                .into()
        } else {
            button(text("Find Devices").size(fs.small))
                .on_press(Message::StartDiscovery)
                .padding([4, 10])
                .style(crate::styles::nav_button)
                .into()
        };

        let discovered_list: Element<'_, Message> = if self.discovered_devices.is_empty() {
            if self.is_discovering {
                text("Scanning network...").size(fs.small).color(dim).into()
            } else if self.discovery_ran {
                // Scan finished with nothing — point the user at the manual
                // IP field above rather than leaving a dead end.
                text("No devices found. Enter the IP address above manually, then set the password and Connect.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.8, 0.6, 0.3))
                    .into()
            } else {
                Space::new().height(0).into()
            }
        } else {
            column(
                self.discovered_devices
                    .iter()
                    .map(|d| {
                        let device = d.clone();
                        let label = format!("{} - {} ({})", d.ip, d.product, d.firmware);
                        button(text(label).size(fs.small))
                            .on_press(Message::SelectDiscoveredDevice(device))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(crate::styles::nav_button)
                            .into()
                    })
                    .collect::<Vec<_>>(),
            )
            .spacing(2)
            .width(Length::Fixed(400.0))
            .into()
        };

        let status_indicator: Element<'_, Message> = if self.status.connected {
            let info_text = self.status.device_info.as_deref().unwrap_or("");
            row![
                text(format!("Connected to {}", self.settings.connection.host))
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                text(info_text).size(fs.small).color(dim),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            text("Not connected")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.3, 0.3))
                .into()
        };

        let connection_section = section!(
            "Connection",
            column![
                row![
                    text("IP Address:").size(fs.normal).color(dim),
                    text_input("eg. 192.168.1.64", &self.host_input)
                        .on_input(Message::HostInputChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(200.0)),
                    discovery_button,
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                discovered_list,
                row![
                    text("Password:").size(fs.normal).color(dim),
                    text_input("optional", &self.password_input)
                        .on_input(Message::PasswordInputChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(200.0)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Stream Control:").size(fs.normal).color(dim),
                    pick_list(
                        &StreamControlMethod::ALL[..],
                        Some(self.settings.connection.stream_control_method),
                        Message::StreamControlMethodChanged,
                    )
                    .text_size(fs.small as f32)
                    .width(Length::Fixed(220.0)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                Space::new().height(4),
                row![
                    button(text("Connect").size(fs.small))
                        .on_press(Message::ConnectPressed)
                        .padding([6, 16])
                        .style(crate::styles::action_button),
                    button(text("Disconnect").size(fs.small))
                        .on_press(Message::DisconnectPressed)
                        .padding([6, 16])
                        .style(crate::styles::nav_button),
                    button(text("Test").size(fs.small))
                        .on_press(Message::RefreshStatus)
                        .padding([6, 16])
                        .style(crate::styles::nav_button),
                    Space::new().width(20),
                    status_indicator,
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Starting Directories ─────────────────────────────────────────
        let fb_dir = self
            .settings
            .default_paths
            .file_browser_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());
        let mp_dir = self
            .settings
            .default_paths
            .music_player_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());

        let dirs_section = section!(
            "Starting Directories",
            column![
                row![
                    text("File Browser:")
                        .size(fs.normal)
                        .color(dim)
                        .width(Length::Fixed(120.0)),
                    text(fb_dir.clone()).size(fs.small).width(Length::Fill),
                    button(text("Browse").size(fs.small))
                        .on_press(Message::BrowseFileBrowserStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                    button(text("Clear").size(fs.small))
                        .on_press(Message::ClearFileBrowserStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Music Player:")
                        .size(fs.normal)
                        .color(dim)
                        .width(Length::Fixed(120.0)),
                    text(mp_dir.clone()).size(fs.small).width(Length::Fill),
                    button(text("Browse").size(fs.small))
                        .on_press(Message::BrowseMusicPlayerStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                    button(text("Clear").size(fs.small))
                        .on_press(Message::ClearMusicPlayerStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                text("Changes take effect on next restart")
                    .size(fs.tiny)
                    .color(dim),
            ]
            .spacing(6)
        );

        // ── Preferences ──────────────────────────────────────────────────
        let prefs_section = section!(
            "Preferences",
            column![
                row![
                    text("Default song duration:").size(fs.normal).color(dim),
                    text_input(
                        "180",
                        &self.settings.preferences.default_song_duration.to_string()
                    )
                    .on_input(Message::DefaultSongDurationChanged)
                    .padding(6)
                    .size(fs.small as f32)
                    .width(Length::Fixed(60.0)),
                    text("seconds").size(fs.small).color(dim),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Font size:").size(fs.normal).color(dim),
                    text_input("12", &self.font_size_input)
                        .on_input(Message::FontSizeChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(50.0)),
                    text("(8\u{2013}24)").size(fs.small).color(dim),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Game library (Game Mode) ─────────────────────────────────────
        // Each configured device folder's subfolders become games in the
        // File Browser's "🎮 Games" launcher.
        let roots = &self.settings.preferences.game_library_roots;
        let mut roots_col = column![].spacing(4);
        if roots.is_empty() {
            roots_col = roots_col.push(
                text("No library folders yet — add a device path like /Usb0/Games")
                    .size(fs.small)
                    .color(dim),
            );
        } else {
            for (i, root) in roots.iter().enumerate() {
                roots_col = roots_col.push(
                    row![
                        text(root.clone()).size(fs.small),
                        Space::new().width(Length::Fill),
                        button(text("Remove").size(fs.tiny))
                            .on_press(Message::GameLibraryRemoveRoot(i))
                            .padding([2, 8])
                            .style(crate::styles::nav_button),
                    ]
                    .align_y(iced::Alignment::Center),
                );
            }
        }
        let game_library_section = section!(
            "Game library",
            column![
                text("Device folders whose subfolders are games in Game Mode.")
                    .size(fs.small)
                    .color(dim),
                roots_col,
                row![
                    text_input("/Usb0/Games", &self.game_library_input)
                        .on_input(Message::GameLibraryInputChanged)
                        .on_submit(Message::GameLibraryAddRoot)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(260.0)),
                    button(text("Add").size(fs.small))
                        .on_press(Message::GameLibraryAddRoot)
                        .padding([4, 12])
                        .style(crate::styles::action_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Debug ────────────────────────────────────────────────────────
        let debug_section = section!(
            "Debug",
            column![text(format!(
                "Platform: {} | Config: {} | Profile: {} ({} total)",
                std::env::consts::OS,
                dirs::config_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                self.profile_manager.active_profile,
                self.profile_manager.profiles.len(),
            ))
            .size(fs.small)
            .color(dim),]
            .spacing(4)
        );

        scrollable(
            column![
                profile_section,
                connection_section,
                dirs_section,
                prefs_section,
                game_library_section,
                debug_section,
            ]
            .spacing(10)
            .padding(15)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }
}
