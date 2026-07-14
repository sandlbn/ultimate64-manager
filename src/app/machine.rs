//! Machine-control handlers (reset / reboot / pause / resume / poweroff / menu)
//! and the shared completion arm. Extracted from `main.rs::update`.

use iced::Task;

use crate::net_utils::REST_TIMEOUT_SECS;
use crate::{Message, Ultimate64Browser, UserMessage};

impl Ultimate64Browser {
    pub(crate) fn handle_reset_machine(&mut self) -> Task<Message> {
        if let Some(conn) = &self.connection {
            let conn = conn.clone();
            Task::perform(
                async move {
                    let result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                        tokio::task::spawn_blocking(move || {
                            let conn = conn.lock().unwrap();
                            conn.reset()
                                .map(|_| "Machine reset successfully".to_string())
                                .map_err(|e| format!("Reset failed: {}", e))
                        }),
                    )
                    .await;

                    match result {
                        Ok(Ok(r)) => r,
                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                        Err(_) => Err("Reset timed out - device may be offline".to_string()),
                    }
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_reboot_machine(&mut self) -> Task<Message> {
        if let Some(host) = &self.host_url {
            let url = format!("{}/v1/machine:reboot", host);
            Task::perform(
                async move {
                    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                    client
                        .put(&url)
                        .send()
                        .await
                        .map_err(|e| format!("Reboot failed: {}", e))?;
                    Ok("Machine rebooting...".to_string())
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_pause_machine(&mut self) -> Task<Message> {
        if let Some(host) = &self.host_url {
            let url = format!("{}/v1/machine:pause", host);
            Task::perform(
                async move {
                    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                    client
                        .put(&url)
                        .send()
                        .await
                        .map_err(|e| format!("Pause failed: {}", e))?;
                    Ok("Machine paused".to_string())
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_resume_machine(&mut self) -> Task<Message> {
        if let Some(host) = &self.host_url {
            let url = format!("{}/v1/machine:resume", host);
            Task::perform(
                async move {
                    let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                    client
                        .put(&url)
                        .send()
                        .await
                        .map_err(|e| format!("Resume failed: {}", e))?;
                    Ok("Machine resumed".to_string())
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_poweroff_machine(&mut self) -> Task<Message> {
        if let Some(conn) = &self.connection {
            let conn = conn.clone();
            Task::perform(
                async move {
                    let result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                        tokio::task::spawn_blocking(move || {
                            let conn = conn.lock().unwrap();
                            conn.poweroff()
                                .map(|_| "Machine powered off".to_string())
                                .map_err(|e| format!("Poweroff failed: {}", e))
                        }),
                    )
                    .await;

                    match result {
                        Ok(Ok(r)) => r,
                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                        Err(_) => Err("Poweroff timed out - device may be offline".to_string()),
                    }
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_menu_button(&mut self) -> Task<Message> {
        if let Some(conn) = &self.connection {
            let conn = conn.clone();
            Task::perform(
                async move {
                    let result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                        tokio::task::spawn_blocking(move || {
                            let conn = conn.lock().unwrap();
                            conn.menu()
                                .map(|_| "Menu button pressed".to_string())
                                .map_err(|e| format!("Menu failed: {}", e))
                        }),
                    )
                    .await;

                    match result {
                        Ok(Ok(r)) => r,
                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                        Err(_) => Err("Menu timed out - device may be offline".to_string()),
                    }
                },
                Message::MachineCommandCompleted,
            )
        } else {
            self.user_message = Some(UserMessage::Error("Not connected".to_string()));
            Task::none()
        }
    }

    pub(crate) fn handle_machine_command_completed(
        &mut self,
        result: Result<String, String>,
    ) -> Task<Message> {
        match result {
            Ok(msg) => {
                self.user_message = Some(UserMessage::Info(msg));
            }
            Err(e) => {
                self.user_message = Some(UserMessage::Error(e));
            }
        }
        Task::none()
    }
}
