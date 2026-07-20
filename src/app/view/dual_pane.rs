//! Dual-pane file browser view. Extracted from .

use iced::widget::{
    button, column, container, pick_list, progress_bar, row, rule, text, text_input, tooltip, Space,
};
use iced::{Element, Length};

use crate::{Message, Pane, Ultimate64Browser};

impl Ultimate64Browser {
    pub(crate) fn view_dual_pane_browser(&self) -> Element<'_, Message> {
        // Left pane - Local files
        let left_content = container(
            self.left_browser
                .view(self.settings.preferences.font_size)
                .map(Message::LeftBrowser),
        )
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(2)
        .style(if self.active_pane == Pane::Left {
            crate::styles::active_pane_style
        } else {
            crate::styles::inactive_pane_style
        });

        let left_pane =
            iced::widget::mouse_area(left_content).on_press(Message::ActivePaneChanged(Pane::Left));

        // Right pane - Ultimate64 files
        let right_content = container(
            self.remote_browser
                .view(self.settings.preferences.font_size)
                .map(Message::RemoteBrowser),
        )
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(2)
        .style(if self.active_pane == Pane::Right {
            crate::styles::active_pane_style
        } else {
            crate::styles::inactive_pane_style
        });

        let right_pane = iced::widget::mouse_area(right_content)
            .on_press(Message::ActivePaneChanged(Pane::Right));

        // Function bar at bottom
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let small = fs.small as f32;
        let tiny = fs.tiny as f32;

        let active_filter = match self.active_pane {
            Pane::Left => self.left_browser.filter(),
            Pane::Right => self.remote_browser.filter(),
        };

        let copy_label = match self.active_pane {
            Pane::Left => "F5 Copy \u{2192}",
            Pane::Right => "F5 Copy \u{2190}",
        };

        let function_bar = container(
            row![
                button(text("F2 Ren").size(small))
                    .on_press(Message::FnRename)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F3 View").size(small))
                    .on_press(Message::FnView)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F4 Edit").size(small))
                    .on_press(Message::FnEdit)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text(copy_label).size(small))
                    .on_press(Message::FnCopy)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F7 MkDir").size(small))
                    .on_press(Message::FnMkDir)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("New Disk").size(small))
                    .on_press(Message::FnNewDisk)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F8 Del").size(small))
                    .on_press(Message::FnDelete)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("↻ Refresh").size(small))
                    .on_press(Message::FnRefresh)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                // Device-control quick actions — gated on connection so an
                // offline click can't fire a hopeless REST request.
                button(text("⏏ Eject A+B").size(small))
                    .on_press_maybe(self.status.connected.then_some(Message::EjectAllDrives),)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                // Run last — re-fires the most recent PRG/CRT/SID/disk
                // the local browser sent. Greys out when nothing's been
                // run yet OR when the device is offline.
                tooltip(
                    button(
                        text(match self.left_browser.last_run() {
                            Some(last) => format!("↪ Run last ({})", last.basename()),
                            None => "↪ Run last".to_string(),
                        })
                        .size(small),
                    )
                    .on_press_maybe(
                        (self.status.connected && self.left_browser.last_run().is_some())
                            .then_some(Message::RunLast),
                    )
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                    text(match self.left_browser.last_run() {
                        Some(last) => format!("Re-run {}", last.path().display()),
                        None => "Nothing has been run yet".to_string(),
                    })
                    .size(tiny),
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip),
                text("|")
                    .size(tiny)
                    .color(iced::Color::from_rgb(0.4, 0.4, 0.45)),
                button(text("Sel All").size(small))
                    .on_press(Message::SelectAllActivePane)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("Sel None").size(small))
                    .on_press(Message::SelectNoneActivePane)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                Space::new().width(Length::Fill),
                text("Filter:")
                    .size(tiny)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                text_input("filter...", active_filter)
                    .on_input(Message::FilterChanged)
                    .size(small)
                    .padding(4)
                    .width(Length::Fixed(120.0)),
                Space::new().width(8),
                pick_list(
                    self.template_manager.get_templates(),
                    self.selected_template.clone(),
                    Message::TemplateSelected,
                )
                .placeholder("Template...")
                .text_size(tiny)
                .width(Length::Fixed(150.0)),
                button(text("Exec").size(tiny))
                    .on_press(Message::ExecuteTemplate)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
            ]
            .spacing(3)
            .align_y(iced::Alignment::Center),
        )
        .padding([5, 8])
        .width(Length::Fill);

        let copy_progress_bar: Element<'_, Message> = {
            let progress_data = self.copy_progress.lock().ok().and_then(|g| g.clone());
            match progress_data {
                Some(p) if !p.done => {
                    let pct = if p.bytes_total > 0 {
                        (p.bytes_transferred as f32 / p.bytes_total as f32).min(1.0)
                    } else if p.total > 0 {
                        p.current as f32 / p.total as f32
                    } else {
                        0.0
                    };
                    // Build label with byte info if available
                    let label = if p.bytes_total > 0 {
                        format!(
                            "{} {}/{} ({})",
                            p.operation,
                            p.current,
                            p.total,
                            crate::file_types::format_file_size(p.bytes_transferred),
                        )
                    } else {
                        format!("{} {}/{}", p.operation, p.current, p.total)
                    };

                    // Calculate ETA based on bytes if available, else items
                    let elapsed = p.started_at.elapsed();
                    let eta_text = if p.bytes_transferred > 0 && p.bytes_total > 0 {
                        let bytes_per_sec = p.bytes_transferred as f64 / elapsed.as_secs_f64();
                        let remaining_bytes =
                            p.bytes_total.saturating_sub(p.bytes_transferred) as f64;
                        let remaining_secs = remaining_bytes / bytes_per_sec;
                        if remaining_secs < 60.0 {
                            format!(
                                "{}/s ~{}s",
                                crate::file_types::format_file_size(bytes_per_sec as u64),
                                remaining_secs as u64
                            )
                        } else {
                            format!(
                                "{}/s ~{}m{}s",
                                crate::file_types::format_file_size(bytes_per_sec as u64),
                                remaining_secs as u64 / 60,
                                remaining_secs as u64 % 60
                            )
                        }
                    } else if p.current > 0 {
                        let secs_per_item = elapsed.as_secs_f64() / p.current as f64;
                        let remaining = p.total.saturating_sub(p.current) as f64 * secs_per_item;
                        if remaining < 60.0 {
                            format!("~{}s left", remaining as u64)
                        } else {
                            format!(
                                "~{}m {}s left",
                                remaining as u64 / 60,
                                remaining as u64 % 60
                            )
                        }
                    } else {
                        "estimating...".to_string()
                    };

                    let file_display = if p.current_file.len() > 25 {
                        format!(
                            "...{}",
                            &p.current_file[p.current_file.len().saturating_sub(22)..]
                        )
                    } else {
                        p.current_file.clone()
                    };

                    container(
                        row![
                            text(label)
                                .size(tiny)
                                .color(iced::Color::from_rgb(0.4, 0.8, 0.4)),
                            text(file_display)
                                .size(tiny)
                                .width(Length::Fixed(150.0))
                                .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                            progress_bar(0.0..=1.0, pct).girth(6.0).length(Length::Fill),
                            text(eta_text)
                                .size(tiny)
                                .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                            button(text("Cancel").size(tiny))
                                .on_press(Message::CopyCancel)
                                .padding([2, 8])
                                .style(crate::styles::nav_button),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding([3, 10])
                    .into()
                }
                _ => Space::new().height(0).into(),
            }
        };

        column![
            row![left_pane, rule::vertical(1), right_pane].height(Length::Fill),
            rule::horizontal(1),
            copy_progress_bar,
            function_bar,
        ]
        .into()
    }
}
