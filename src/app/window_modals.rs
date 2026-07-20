//! Window management, overlay/modal lifecycle (help, eject, close, toast),
//! and Esc/fullscreen handling. Extracted from `main.rs::update`.

use iced::Task;

use iced::window;

use crate::api;
use crate::file_browser::FileBrowserMessage;
use crate::{Message, Ultimate64Browser, UserMessage, TOAST_DURATION_SECS};

impl Ultimate64Browser {
    pub(crate) fn handle_close_streaming_window(&mut self) -> Task<Message> {
        if let Some(id) = self.streaming_window_id {
            self.streaming_window_id = None;
            return iced::window::close(id);
        }
        Task::none()
    }

    pub(crate) fn handle_dismiss_message(&mut self) -> Task<Message> {
        self.user_message = None;
        Task::none()
    }

    pub(crate) fn handle_esc_pressed(&mut self) -> Task<Message> {
        // Priority order: dismiss the most-modal thing first.
        // Help overlay → eject confirm → drop dialog → fullscreen → pane quick-search.
        if self.show_help {
            self.show_help = false;
            return Task::none();
        }
        if self.pending_close.is_some() {
            self.pending_close = None;
            return Task::none();
        }
        if self.pending_eject_confirm {
            self.pending_eject_confirm = false;
            return Task::none();
        }
        if self.pending_drop.is_some() {
            self.pending_drop = None;
            return Task::none();
        }
        if self.video_streaming.is_fullscreen {
            return self.update(Message::ExitFullscreen);
        }
        // Final fallback: clear the active pane's quick-search buffer.
        self.dispatch_local_pane_message(FileBrowserMessage::QuickSearchClear)
    }

    pub(crate) fn handle_exit_fullscreen(&mut self) -> Task<Message> {
        // Only exit fullscreen if currently in fullscreen mode
        if self.video_streaming.is_fullscreen {
            self.video_streaming.is_fullscreen = false;
            // Exit fullscreen on the appropriate window
            if let Some(streaming_id) = self.streaming_window_id {
                return window::set_mode(streaming_id, iced::window::Mode::Windowed)
                    .map(|_: ()| Message::RefreshStatus);
            } else {
                return iced::window::oldest()
                    .and_then(|id| iced::window::set_mode(id, iced::window::Mode::Windowed))
                    .map(|_: ()| Message::RefreshStatus);
            }
        }
        Task::none()
    }

    pub(crate) fn handle_open_streaming_window(&mut self) -> Task<Message> {
        if self.streaming_window_id.is_some() {
            // Window already open
            return Task::none();
        }
        let settings = iced::window::Settings {
            size: iced::Size::new(800.0, 600.0),
            min_size: Some(iced::Size::new(400.0, 300.0)),
            decorations: true,
            ..Default::default()
        };
        // Destructure the tuple - open() returns (Id, Task<Id>)
        let (id, open_task) = iced::window::open(settings);
        self.streaming_window_id = Some(id);
        open_task.map(move |_| Message::StreamingWindowOpened(id))
    }

    pub(crate) fn handle_streaming_window_opened(&mut self, id: iced::window::Id) -> Task<Message> {
        log::info!("Streaming window opened: {:?}", id);
        // ID already stored in OpenStreamingWindow handler
        Task::none()
    }

    pub(crate) fn handle_window_close_requested(&mut self, id: iced::window::Id) -> Task<Message> {
        // Streaming window — close it without prompting; only the
        // main window holds in-flight transfers that we'd hate to
        // lose.
        if self.streaming_window_id == Some(id) {
            return iced::window::close(id);
        }
        if self.main_window_id == Some(id) && self.is_transfer_in_flight() {
            self.pending_close = Some(id);
            Task::none()
        } else {
            iced::window::close(id)
        }
    }

    pub(crate) fn handle_confirm_close_window(&mut self) -> Task<Message> {
        if let Some(id) = self.pending_close.take() {
            iced::window::close(id)
        } else {
            Task::none()
        }
    }

    pub(crate) fn handle_cancel_close_window(&mut self) -> Task<Message> {
        self.pending_close = None;
        Task::none()
    }

    pub(crate) fn handle_window_closed(&mut self, id: iced::window::Id) -> Task<Message> {
        if self.streaming_window_id == Some(id) {
            // Streaming window was closed
            log::info!("Streaming window closed: {:?}", id);
            self.streaming_window_id = None;
            Task::none()
        } else if self.main_window_id == Some(id) {
            // Main window was closed - clean up immediately and exit
            log::info!("Main window closed: {:?}", id);

            // Cancel any in-progress copy transfer and clear it
            if let Ok(mut g) = self.copy_progress.lock() {
                if let Some(ref mut p) = *g {
                    p.cancelled = true;
                    p.done = true;
                }
            }

            // Cancel any remote browser transfer
            self.remote_browser.cancel_transfer();

            // Stop streaming if active
            if self.video_streaming.is_streaming {
                self.video_streaming
                    .stop_signal
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }

            // Disconnect immediately to prevent further status checks
            self.connection = None;
            self.host_url = None;
            self.status.connected = false;

            // Mark main window as gone so subscriptions stop
            self.main_window_id = None;

            // Close any remaining windows and exit
            if let Some(streaming_id) = self.streaming_window_id {
                self.streaming_window_id = None;
                return Task::batch(vec![iced::window::close(streaming_id), iced::exit()]);
            }
            iced::exit()
        } else {
            Task::none()
        }
    }

    pub(crate) fn handle_show_help(&mut self) -> Task<Message> {
        self.show_help = true;
        Task::none()
    }

    pub(crate) fn handle_hide_help(&mut self) -> Task<Message> {
        self.show_help = false;
        Task::none()
    }

    pub(crate) fn handle_toast_tick(&mut self) -> Task<Message> {
        if let Some((_, shown_at)) = &self.toast {
            if shown_at.elapsed() >= std::time::Duration::from_secs(TOAST_DURATION_SECS) {
                self.toast = None;
            }
        }
        Task::none()
    }

    pub(crate) fn handle_eject_all_drives(&mut self) -> Task<Message> {
        // Click on the toolbar button arms the confirmation modal —
        // the actual ejection is gated on EjectAllDrivesConfirmed
        // so an accidental click can't clear a hand-set mount.
        self.pending_eject_confirm = true;
        Task::none()
    }

    pub(crate) fn handle_eject_cancel(&mut self) -> Task<Message> {
        self.pending_eject_confirm = false;
        Task::none()
    }

    pub(crate) fn handle_eject_all_drives_confirmed(&mut self) -> Task<Message> {
        self.pending_eject_confirm = false;
        let host_url = format!("http://{}", self.settings.connection.host);
        let password = self.settings.connection.password.clone();
        // Fire both unmount calls in parallel; the device handles
        // them independently. Each has its own 5s REST timeout
        // baked into the client, so the whole op is bounded.
        let host_a = host_url.clone();
        let pwd_a = password.clone();
        let host_b = host_url;
        let pwd_b = password;
        Task::perform(
            async move {
                let (res_a, res_b) = tokio::join!(
                    api::unmount_disk_async(&host_a, "a", pwd_a.as_deref()),
                    api::unmount_disk_async(&host_b, "b", pwd_b.as_deref()),
                );
                match (res_a, res_b) {
                    (Ok(()), Ok(())) => Ok("Ejected Drives A and B".to_string()),
                    (Ok(()), Err(e)) => Err(format!("Drive A OK, Drive B failed: {}", e)),
                    (Err(e), Ok(())) => Err(format!("Drive B OK, Drive A failed: {}", e)),
                    (Err(a), Err(b)) => Err(format!("Both drives failed: {}; {}", a, b)),
                }
            },
            Message::EjectCompleted,
        )
    }

    pub(crate) fn handle_eject_completed(
        &mut self,
        result: Result<String, String>,
    ) -> Task<Message> {
        if let Ok(msg) = &result {
            self.show_toast(msg.clone());
        }
        self.user_message = Some(match result {
            Ok(msg) => UserMessage::Info(msg),
            Err(e) => UserMessage::Error(e),
        });
        Task::none()
    }
}
