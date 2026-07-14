//! Connection lifecycle: connect/disconnect, status polling, and the
//! post-connect refresh. Extracted from `main.rs::update`.

use iced::Task;

use crate::fetch_status;
use crate::remote_browser::RemoteBrowserMessage;
use crate::settings::{ConnectionSettings, StreamControlMethod};
use crate::streaming::StreamingMessage;
use crate::tab::{TabContext, TabController};
use crate::{Message, StatusInfo, Ultimate64Browser, UserMessage, MAX_TRANSIENT_STATUS_FAILURES};

impl Ultimate64Browser {
    pub(crate) fn handle_host_input_changed(&mut self, value: String) -> Task<Message> {
        self.host_input = value;
        Task::none()
    }

    pub(crate) fn handle_password_input_changed(&mut self, value: String) -> Task<Message> {
        self.password_input = value;
        Task::none()
    }

    pub(crate) fn handle_connect_pressed(&mut self) -> Task<Message> {
        log::info!("Connect button pressed");
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
        self.settings = self.profile_manager.active_settings().clone();

        self.establish_connection();
        // Trigger status refresh and remote browser refresh after a short delay
        Task::perform(
            async {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            },
            |_| Message::RefreshAfterConnect,
        )
    }

    pub(crate) fn handle_disconnect_pressed(&mut self, ctx: TabContext) -> Task<Message> {
        log::info!("Disconnecting...");

        // Stop video streaming if active to prevent hangs
        if self.video_streaming.is_streaming {
            let _ = self
                .video_streaming
                .update(StreamingMessage::StopStream, ctx.clone());
        }

        self.connection = None;
        self.host_url = None;
        self.status.connected = false;
        self.status.device_info = None;
        self.status.mounted_disks.clear();
        self.consecutive_status_failures = 0;
        self.remote_browser.set_host(None, None);
        // Clear telnet host for video streaming control
        self.video_streaming.set_ultimate_host(None);
        self.user_message = Some(UserMessage::Info(
            "Disconnected from Ultimate64".to_string(),
        ));
        Task::none()
    }

    pub(crate) fn handle_stream_control_method_changed(
        &mut self,
        method: StreamControlMethod,
    ) -> Task<Message> {
        self.profile_manager
            .active_settings_mut()
            .connection
            .stream_control_method = method;
        self.settings = self.profile_manager.active_settings().clone();
        self.video_streaming.set_stream_control_method(method);
        if let Err(e) = self.profile_manager.save() {
            log::error!("Failed to save profiles: {}", e);
        }
        Task::none()
    }

    pub(crate) fn handle_refresh_status(&mut self) -> Task<Message> {
        if let Some(conn) = &self.connection {
            let conn = conn.clone();
            Task::perform(
                async move { fetch_status(conn).await },
                Message::StatusUpdated,
            )
        } else {
            Task::none()
        }
    }

    pub(crate) fn handle_refresh_after_connect(&mut self, ctx: TabContext) -> Task<Message> {
        // Refresh both status and remote browser after connection
        let status_cmd = if let Some(conn) = &self.connection {
            let conn = conn.clone();
            Task::perform(
                async move { fetch_status(conn).await },
                Message::StatusUpdated,
            )
        } else {
            Task::none()
        };

        let browser_cmd = self
            .remote_browser
            .update(RemoteBrowserMessage::RefreshFiles, ctx.clone())
            .map(Message::RemoteBrowser);

        Task::batch(vec![status_cmd, browser_cmd])
    }

    pub(crate) fn handle_status_updated(
        &mut self,
        result: Result<StatusInfo, String>,
        ctx: TabContext,
    ) -> Task<Message> {
        match result {
            Ok(status) => {
                log::debug!(
                    "Status: Connected={}, Device={:?}, Disks={}",
                    status.connected,
                    status.device_info,
                    status.mounted_disks.len()
                );
                // Recovered from a transient outage — log it so the
                // user can correlate with reboot events.
                if self.consecutive_status_failures > 0 {
                    log::info!(
                        "Status recovered after {} failed poll(s)",
                        self.consecutive_status_failures
                    );
                }
                self.consecutive_status_failures = 0;
                // Show connected message when first connecting
                if !self.status.connected && status.connected {
                    self.user_message = Some(UserMessage::Info(format!(
                        "Connected to {}",
                        self.settings.connection.host
                    )));
                }
                self.status = status;
            }
            Err(e) => {
                if self.remote_browser.is_transferring() {
                    log::debug!("Ignoring status failure during active transfer");
                    return Task::none();
                }
                // Ignore status failures during profile operations — the
                // device may be rebooting or applying config, which
                // legitimately takes it offline for 15-30 seconds.
                if self.device_profile_manager.is_loading {
                    log::debug!("Ignoring status failure during profile operation");
                    return Task::none();
                }

                self.consecutive_status_failures =
                    self.consecutive_status_failures.saturating_add(1);

                // Tolerate brief outages (reboots typically settle in
                // 2-3 seconds) — keep the UI showing "Connected" and
                // let the subscription poll back at the faster cadence.
                if self.consecutive_status_failures < MAX_TRANSIENT_STATUS_FAILURES {
                    log::debug!(
                        "Status poll failed ({} of {}): {}",
                        self.consecutive_status_failures,
                        MAX_TRANSIENT_STATUS_FAILURES,
                        e
                    );
                    return Task::none();
                }

                log::warn!(
                    "Status update failed {} times — marking disconnected: {}",
                    self.consecutive_status_failures,
                    e
                );
                self.status.connected = false;
                self.status.device_info = None;
                // Stop streaming only if it was running
                if self.video_streaming.is_streaming {
                    let _ = self
                        .video_streaming
                        .update(StreamingMessage::StopStream, ctx.clone());
                }
            }
        }
        Task::none()
    }
}
