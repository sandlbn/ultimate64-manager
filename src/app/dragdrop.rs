//! Drag-and-drop handlers: the quick-action dialog and its dispatch.
//! Extracted from `main.rs::update`.

use iced::Task;
use std::path::PathBuf;

use crate::api;
use crate::basic_editor::BasicEditorMessage;
use crate::ftp_ops;
use crate::{DropAction, Message, Tab, Ultimate64Browser, UserMessage};

impl Ultimate64Browser {
    pub(crate) fn handle_file_dropped(&mut self, path: PathBuf) -> Task<Message> {
        // If a drop is already pending or one is in flight, ignore
        // the new one — the user can re-drop after dismissing the
        // dialog. Avoids the dialog stacking on accidental drops.
        if self.pending_drop.is_none() && !self.drop_in_flight {
            log::info!("File dropped: {}", path.display());
            self.pending_drop = Some(path);
        }
        Task::none()
    }

    pub(crate) fn handle_drop_cancel(&mut self) -> Task<Message> {
        self.pending_drop = None;
        Task::none()
    }

    pub(crate) fn handle_drop_action(&mut self, action: DropAction) -> Task<Message> {
        self.pending_drop = None;
        self.user_message = Some(UserMessage::Info(format!("{}…", action.status_label())));
        let host_url = format!("http://{}", self.settings.connection.host);
        let password = self.settings.connection.password.clone();
        let remote_path = self.remote_browser.current_path.clone();
        // Only network actions need the cancel handle + in-flight
        // flag — OpenInBasicEditor is local fs reads that finish
        // in milliseconds and don't deserve a Cancel button.
        let is_network = !matches!(action, DropAction::OpenInBasicEditor { .. });
        self.drop_in_flight = is_network;
        let task = match action {
            DropAction::RunOnDevice { path, runner } => Task::perform(
                async move {
                    let bytes = tokio::fs::read(&path)
                        .await
                        .map_err(|e| format!("Read failed: {}", e))?;
                    tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        api::upload_runner_async(&host_url, runner, bytes, password.as_deref()),
                    )
                    .await
                    .map_err(|_| "Send timed out — device offline?".to_string())?
                    .map(|_| {
                        format!(
                            "Sent {} via {}",
                            path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
                            runner
                        )
                    })
                },
                Message::DropCompleted,
            ),
            DropAction::MountDisk { path } => {
                let drive = "a"; // dropped images mount on the active drive A
                Task::perform(
                    async move {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(30),
                            api::upload_mount_disk_async(
                                &host_url,
                                &path,
                                drive,
                                "readonly",
                                password.as_deref(),
                            ),
                        )
                        .await
                        .map_err(|_| "Mount timed out — device offline?".to_string())?
                        .map(|_| {
                            format!(
                                "Mounted {} on Drive A (RO)",
                                path.file_name().and_then(|s| s.to_str()).unwrap_or("disk")
                            )
                        })
                    },
                    Message::DropCompleted,
                )
            }
            DropAction::OpenInBasicEditor { path } => {
                // Local file load — no network. Switch to the BASIC
                // tab and reuse the editor's existing OpenCompleted
                // message so it gets the same treatment as Open .bas.
                self.active_tab = Tab::BasicEditor;
                Task::perform(
                    async move {
                        let text = tokio::fs::read_to_string(&path)
                            .await
                            .map_err(|e| format!("Read failed: {}", e))?;
                        Ok::<_, String>((path, text))
                    },
                    |result| Message::BasicEditor(BasicEditorMessage::OpenCompleted(result)),
                )
            }
            DropAction::UploadToRemote { path } => {
                let progress = self.remote_browser.transfer_progress_handle();
                // FTP connect needs a bare host, no scheme — the
                // user-configured host might include `http://`.
                let host = self
                    .settings
                    .connection
                    .host
                    .trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .trim_end_matches('/')
                    .to_string();
                // `upload_file_ftp` only treats `remote_dest` as a
                // directory when it ends with `/`. The remote
                // browser's `current_path` is `/SD` (no trailing
                // slash) once the user navigates anywhere, so we
                // append one explicitly to avoid an empty CWD.
                let dest = if remote_path.ends_with('/') {
                    remote_path
                } else {
                    format!("{}/", remote_path)
                };
                Task::perform(
                    async move {
                        ftp_ops::upload_file_ftp(host, path.clone(), dest, password, progress)
                            .await
                            .map(|_| {
                                format!(
                                    "Uploaded {}",
                                    path.file_name().and_then(|s| s.to_str()).unwrap_or("file")
                                )
                            })
                    },
                    Message::DropCompleted,
                )
            }
        };
        // Wrap the network task in `abortable()` so the Cancel
        // button can drop the future without waiting for the
        // timeout. Local-only tasks (OpenInBasicEditor) skip this
        // — they have nothing to cancel.
        if is_network {
            let (task, handle) = task.abortable();
            self.drop_handle = Some(handle);
            task
        } else {
            task
        }
    }

    pub(crate) fn handle_drop_completed(
        &mut self,
        result: Result<String, String>,
    ) -> Task<Message> {
        self.drop_in_flight = false;
        self.drop_handle = None;
        match &result {
            Ok(msg) => self.show_toast(msg.clone()),
            Err(_) => {}
        }
        self.user_message = Some(match result {
            Ok(msg) => UserMessage::Info(msg),
            Err(e) => UserMessage::Error(e),
        });
        Task::none()
    }

    pub(crate) fn handle_drop_abort(&mut self) -> Task<Message> {
        if let Some(h) = self.drop_handle.take() {
            h.abort();
        }
        self.drop_in_flight = false;
        self.user_message = Some(UserMessage::Info("Drop cancelled".into()));
        Task::none()
    }
}
