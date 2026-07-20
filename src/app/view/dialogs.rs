//! Modal/overlay and connection-bar views. Extracted from .

use iced::widget::{button, column, container, row, text, Space};
use iced::{Element, Length};

use crate::{DropAction, Message, PendingCopy, Ultimate64Browser, HELP_BINDS};
use std::path::PathBuf;

impl Ultimate64Browser {
    pub(crate) fn view_help_overlay(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let header = text("Keyboard Shortcuts").size(fs.large);
        let mut body = column![header, Space::new().height(10)].spacing(0);

        let mut current_section: Option<&'static str> = None;
        for (section, key, desc) in HELP_BINDS {
            if Some(*section) != current_section {
                if current_section.is_some() {
                    body = body.push(Space::new().height(8));
                }
                body = body.push(
                    text(*section)
                        .size(fs.normal)
                        .color(iced::Color::from_rgb(0.45, 0.65, 1.00)),
                );
                body = body.push(Space::new().height(4));
                current_section = Some(section);
            }
            body = body.push(
                row![
                    container(
                        text(*key)
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.85, 0.75, 0.45))
                    )
                    .width(Length::Fixed(180.0)),
                    text(*desc)
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.85, 0.85, 0.9)),
                ]
                .spacing(8),
            );
        }

        body = body.push(Space::new().height(14));
        body = body.push(
            row![
                Space::new().width(Length::Fill),
                button(text("Close").size(fs.small))
                    .on_press(Message::HideHelp)
                    .padding([6, 14]),
                Space::new().width(Length::Fill),
            ]
            .align_y(iced::Alignment::Center),
        );
        body = body.push(
            text("Press Esc to close")
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
        );

        let dialog = container(body.padding(20))
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba(
                    0.18, 0.20, 0.28, 0.98,
                ))),
                border: iced::Border {
                    color: iced::Color::from_rgba(0.45, 0.52, 0.85, 0.7),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            })
            .width(Length::Fixed(520.0));

        container(dialog)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(20)
            .into()
    }
    pub(crate) fn view_drop_dialog(&self, path: &PathBuf) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let size_label = std::fs::metadata(path)
            .ok()
            .map(|m| crate::file_types::format_file_size(m.len()))
            .unwrap_or_else(|| "?".to_string());
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let header = text(format!("Dropped: {}  ({})", basename, size_label)).size(fs.normal);
        let path_line = text(path.display().to_string())
            .size(fs.tiny)
            .color(iced::Color::from_rgb(0.55, 0.55, 0.6));

        // Per-extension actions (may be empty for unknown types).
        let mut button_col = column![].spacing(8);
        for action in DropAction::available_for(&ext, path) {
            let label = action.button_label();
            button_col = button_col.push(
                button(text(label).size(fs.normal))
                    .on_press(Message::DropAction(action))
                    .padding([6, 14])
                    .width(Length::Fill),
            );
        }
        // Upload to remote — always available.
        button_col = button_col.push(
            button(
                text(DropAction::UploadToRemote { path: path.clone() }.button_label())
                    .size(fs.normal),
            )
            .on_press(Message::DropAction(DropAction::UploadToRemote {
                path: path.clone(),
            }))
            .padding([6, 14])
            .width(Length::Fill),
        );
        button_col = button_col.push(
            button(text("✕ Cancel").size(fs.normal))
                .on_press(Message::DropCancel)
                .padding([6, 14])
                .width(Length::Fill)
                .style(iced::widget::button::text),
        );

        let dialog = container(
            column![header, path_line, Space::new().height(8), button_col]
                .spacing(6)
                .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.45, 0.52, 0.85, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(420.0));

        // Click-outside-to-dismiss: the outer mouse_area covers the full
        // backdrop and fires DropCancel; the inner mouse_area around the
        // dialog absorbs clicks (with a Nop) so clicks ON the dialog itself
        // — between buttons, on the border, etc. — don't bubble through.
        // Button widgets capture their own clicks already, so this only
        // affects "dead" space inside the dialog.
        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::DropCancel)
        .into()
    }
    pub(crate) fn view_eject_confirm_dialog(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let dialog = container(
            column![
                text("Eject both drives?").size(fs.large),
                text("Drive A and Drive B will be cleared on the device. Any in-progress writes finish first; this cannot be undone.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
                Space::new().height(12),
                column![
                    button(text("⏏ Yes, eject A+B").size(fs.normal))
                        .on_press(Message::EjectAllDrivesConfirmed)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::danger),
                    button(text("Cancel").size(fs.normal))
                        .on_press(Message::EjectCancel)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::text),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.85, 0.4, 0.4, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(380.0));

        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::EjectCancel)
        .into()
    }
    pub(crate) fn view_close_confirm_dialog(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let dialog = container(
            column![
                text("Quit while transfer is in progress?").size(fs.large),
                text("A file transfer or download hasn't finished yet. Closing now will abort it; partial files may be left behind.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
                Space::new().height(12),
                column![
                    button(text("Quit anyway").size(fs.normal))
                        .on_press(Message::ConfirmCloseWindow)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::danger),
                    button(text("Keep working").size(fs.normal))
                        .on_press(Message::CancelCloseWindow)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::text),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.85, 0.4, 0.4, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(420.0));

        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::CancelCloseWindow)
        .into()
    }
    pub(crate) fn view_overwrite_dialog(&self, pending: &PendingCopy) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let n = pending.conflicts.len();
        let header = if n == 1 {
            format!("Overwrite 1 file in {}?", pending.remote_dest)
        } else {
            format!("Overwrite {} files in {}?", n, pending.remote_dest)
        };

        let mut list_col = column![].spacing(2);
        for name in pending.conflicts.iter().take(8) {
            list_col = list_col.push(
                text(format!("  • {}", name))
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
            );
        }
        if n > 8 {
            list_col = list_col.push(
                text(format!("  … and {} more", n - 8))
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
            );
        }

        container(
            column![
                text("⚠ Overwrite existing files")
                    .size(fs.large)
                    .color(iced::Color::from_rgb(1.0, 0.6, 0.3)),
                Space::new().height(8),
                text(header).size(fs.normal),
                Space::new().height(6),
                list_col,
                Space::new().height(12),
                text("Existing files with the same name will be replaced.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.9, 0.5, 0.5)),
                Space::new().height(16),
                row![
                    button(text("Cancel").size(fs.normal))
                        .on_press(Message::CopyOverwriteCancel)
                        .padding([6, 20])
                        .style(button::secondary),
                    Space::new().width(12),
                    button(text("Overwrite").size(fs.normal))
                        .on_press(Message::CopyOverwriteConfirm)
                        .padding([6, 20])
                        .style(button::danger),
                ]
                .align_y(iced::Alignment::Center),
            ]
            .align_x(iced::Alignment::Center)
            .spacing(2),
        )
        .padding(40)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }
    pub(crate) fn view_connection_bar(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let status_indicator = if self.status.connected {
            text("● CONNECTED").color(iced::Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            text("○ DISCONNECTED").color(iced::Color::from_rgb(0.8, 0.2, 0.2))
        };

        let device_text =
            text(self.status.device_info.as_deref().unwrap_or("No device")).size(fs.normal);

        // Update notification on the right side
        let update_notification: Element<'_, Message> = if let Some(info) = &self.new_version {
            row![
                text(format!("🎉 {} available!", info.version))
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                button(text("Download").size(fs.small))
                    .on_press(Message::OpenReleasePage)
                    .padding([2, 8])
                    .style(button::primary),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            Space::new().into()
        };

        container(
            row![
                status_indicator,
                text(" | ").size(fs.normal),
                device_text,
                Space::new().width(Length::Fill),
                update_notification,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
    }
}
