//! Bottom status/control bar view. Extracted from .

use iced::widget::{button, container, row, text, tooltip, Space};
use iced::{Element, Length};

use crate::streaming::StreamingMessage;
use crate::{Message, Ultimate64Browser, UserMessage};

impl Ultimate64Browser {
    pub(crate) fn view_status_bar(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let video_status = if self.video_streaming.is_streaming {
            "STREAMING"
        } else {
            "IDLE"
        };

        // Show user message if present, otherwise show video status
        let status_text: Element<'_, Message> = if let Some(msg) = &self.user_message {
            let (prefix, message, is_error) = match msg {
                UserMessage::Error(e) => ("ERROR: ", e.as_str(), true),
                UserMessage::Info(i) => ("", i.as_str(), false),
            };
            let color = if is_error {
                iced::Color::from_rgb(0.8, 0.0, 0.0)
            } else {
                iced::Color::from_rgb(0.0, 0.5, 0.0)
            };

            // Check if this is a screenshot message - make path clickable
            if message.starts_with("Screenshot saved: ") {
                let path = message
                    .strip_prefix("Screenshot saved: ")
                    .unwrap_or(message);
                row![
                    text("Screenshot saved: ").size(fs.normal).color(color),
                    button(
                        text(path)
                            .size(fs.normal)
                            .color(iced::Color::from_rgb(0.3, 0.6, 1.0))
                    )
                    .style(button::text)
                    .on_press(Message::Streaming(StreamingMessage::OpenScreenshot(
                        path.to_string()
                    )))
                    .padding(0),
                    tooltip(
                        button(text("X").size(fs.tiny))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                let mut row_items: Vec<Element<'_, Message>> =
                    vec![text(format!("{}{}", prefix, message))
                        .size(fs.normal)
                        .color(color)
                        .into()];
                // While a drag-and-drop upload is in flight, expose a
                // Cancel button right next to the status text so the user
                // doesn't have to wait for the timeout if the device is
                // silent. Cancels via `Task::abort()` on the stashed handle.
                if self.drop_in_flight {
                    row_items.push(
                        tooltip(
                            button(text("✕ Cancel").size(fs.tiny))
                                .on_press(Message::DropAbort)
                                .padding([2, 8]),
                            "Cancel the in-flight drop action",
                            tooltip::Position::Top,
                        )
                        .style(container::bordered_box)
                        .into(),
                    );
                }
                row_items.push(
                    tooltip(
                        button(text("X").size(fs.tiny))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into(),
                );
                iced::widget::Row::with_children(row_items)
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
            }
        } else {
            text(video_status).size(fs.normal).into()
        };

        // Enable machine control whenever a connection exists, rather than
        // only when the last status poll succeeded — a transient poll failure
        // on a reachable device shouldn't disable Reset/Reboot/etc.
        let connected = self.connection.is_some();

        container(
            row![
                status_text,
                Space::new().width(Length::Fill),
                tooltip(
                    button(text("MENU").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::MenuButton))
                        .padding([4, 8]),
                    "Press Ultimate64 menu button",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text("|").size(fs.normal),
                tooltip(
                    button(text("PAUSE").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::PauseMachine))
                        .padding([4, 8]),
                    "Pause the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("RESUME").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::ResumeMachine))
                        .padding([4, 8]),
                    "Resume the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text("|").size(fs.normal),
                tooltip(
                    button(text("RESET").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::ResetMachine))
                        .padding([4, 8]),
                    "Reset the C64 (soft reset)",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("REBOOT").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::RebootMachine))
                        .padding([4, 8]),
                    "Reboot the Ultimate64 device",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("POWER OFF").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::PoweroffMachine))
                        .padding([4, 8]),
                    "Power off the Ultimate64",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
    }
}
