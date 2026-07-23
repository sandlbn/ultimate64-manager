//! DEVICE tab handlers — drive control, keyboard injection, debug register,
//! and the debug bus-trace stream capture. Extracted from `main.rs::update`.

use iced::Task;

use crate::api;
use crate::debug_stream;
use crate::{Message, Ultimate64Browser, UserMessage};

impl Ultimate64Browser {
    /// Re-poll device status shortly after a drive change so the drive-control
    /// strip reflects the new type/power. `RefreshStatus` is a no-op when
    /// disconnected, so this only does work while connected.
    fn refresh_drives_soon() -> Task<Message> {
        Task::perform(
            async {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            },
            |_| Message::RefreshStatus,
        )
    }

    pub(crate) fn handle_device_set_drive_mode(
        &mut self,
        drive: String,
        mode: String,
    ) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        let password = self.settings.connection.password.clone();
        Task::batch([
            Task::perform(
                async move {
                    api::set_drive_mode_async(&host, &drive, &mode, password.as_deref()).await
                },
                Message::MachineCommandCompleted,
            ),
            Self::refresh_drives_soon(),
        ])
    }

    pub(crate) fn handle_device_drive_power(&mut self, drive: String, on: bool) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        let password = self.settings.connection.password.clone();
        Task::batch([
            Task::perform(
                async move { api::drive_power_async(&host, &drive, on, password.as_deref()).await },
                Message::MachineCommandCompleted,
            ),
            Self::refresh_drives_soon(),
        ])
    }

    pub(crate) fn handle_device_drive_reset(&mut self, drive: String) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        let password = self.settings.connection.password.clone();
        Task::batch([
            Task::perform(
                async move { api::drive_reset_async(&host, &drive, password.as_deref()).await },
                Message::MachineCommandCompleted,
            ),
            Self::refresh_drives_soon(),
        ])
    }

    pub(crate) fn handle_device_send_keys(&mut self) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        if self.device_keyboard_input.is_empty() {
            return Task::none();
        }
        let password = self.settings.connection.password.clone();
        // Append RETURN so a typed command actually executes. This writes
        // PETSCII into the KERNAL keyboard buffer ($0277) — only code that
        // reads via the KERNAL (BASIC prompt, GETIN) sees it; programs
        // scanning the CIA keyboard matrix directly do not.
        let text = format!("{}\n", self.device_keyboard_input);
        Task::perform(
            async move {
                api::type_text_async(&host, &text, password.as_deref())
                    .await
                    .map(|_| "Sent keystrokes".to_string())
            },
            Message::MachineCommandCompleted,
        )
    }

    pub(crate) fn handle_device_read_debugreg(&mut self) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        let password = self.settings.connection.password.clone();
        Task::perform(
            async move { api::read_debugreg_async(&host, password.as_deref()).await },
            Message::DeviceDebugRegRead,
        )
    }

    pub(crate) fn handle_device_debugreg_read(
        &mut self,
        result: Result<u8, String>,
    ) -> Task<Message> {
        match result {
            Ok(v) => {
                self.device_debugreg_value = Some(v);
                self.user_message = Some(UserMessage::Info(format!("Debug register = ${:02X}", v)));
            }
            Err(e) => {
                self.user_message = Some(UserMessage::Error(e));
            }
        }
        Task::none()
    }

    pub(crate) fn handle_device_write_debugreg(&mut self) -> Task<Message> {
        let Some(host) = self.host_url.clone() else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            return Task::none();
        };
        // Accept hex with optional 0x/$ prefix.
        let raw = self.device_debugreg_input.trim();
        let raw = raw
            .strip_prefix("0x")
            .or_else(|| raw.strip_prefix('$'))
            .unwrap_or(raw);
        let Ok(value) = u8::from_str_radix(raw, 16) else {
            self.user_message = Some(UserMessage::Error(
                "Debug register: enter a hex byte (00–FF)".to_string(),
            ));
            return Task::none();
        };
        let password = self.settings.connection.password.clone();
        Task::perform(
            async move { api::write_debugreg_async(&host, value, password.as_deref()).await },
            Message::MachineCommandCompleted,
        )
    }

    pub(crate) fn handle_device_debug_stream_start(&mut self) -> Task<Message> {
        self.debug_stream.set_host(
            self.host_url.clone(),
            self.settings.connection.password.clone(),
            self.settings.connection.stream_control_method,
        );
        match self.debug_stream.start() {
            Ok(()) => {
                self.user_message = Some(UserMessage::Info(
                    "Debug stream capture started".to_string(),
                ));
            }
            Err(e) => {
                self.user_message = Some(UserMessage::Error(format!("Debug stream: {}", e)));
            }
        }
        Task::none()
    }

    pub(crate) fn handle_device_debug_stream_stop(&mut self) -> Task<Message> {
        self.debug_stream.stop();
        self.user_message = Some(UserMessage::Info(format!(
            "Debug stream stopped — {} packets captured",
            self.debug_stream.packets()
        )));
        Task::none()
    }

    pub(crate) fn handle_device_debug_stream_save(&mut self) -> Task<Message> {
        let data = self.debug_stream.snapshot();
        Task::perform(
            debug_stream::save_capture_async(data),
            Message::DeviceDebugStreamSaved,
        )
    }

    pub(crate) fn handle_device_debug_stream_saved(
        &mut self,
        result: Result<String, String>,
    ) -> Task<Message> {
        match result {
            Ok(path) => {
                self.user_message = Some(UserMessage::Info(format!("Capture saved: {}", path)));
            }
            Err(e) => {
                self.user_message = Some(UserMessage::Error(e));
            }
        }
        Task::none()
    }
}
