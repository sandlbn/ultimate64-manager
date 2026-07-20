//! File-copy handlers (local↔remote), including the overwrite gate and
//! progress/completion arms. Extracted from `main.rs::update`.

use iced::Task;

use crate::file_browser::FileBrowserMessage;
use crate::remote_browser::RemoteBrowserMessage;
use crate::tab::{TabContext, TabController};
use crate::{
    count_remote_files_recursive, download_directory_with_progress, Message, PendingCopy,
    Ultimate64Browser, UserMessage,
};

impl Ultimate64Browser {
    pub(crate) fn handle_copy_local_to_remote(&mut self) -> Task<Message> {
        // Copy checked local files and directories to Ultimate64.
        // Before kicking off the FTP upload, check which top-level
        // destination names already exist on the device — if any do,
        // stash the operation in `pending_copy` and let the overwrite
        // dialog gate the actual transfer.
        let items_to_copy = self.left_browser.get_checked_files();

        if items_to_copy.is_empty() {
            self.user_message = Some(UserMessage::Error(
                "No files selected. Use checkboxes to select files.".to_string(),
            ));
            return Task::none();
        }

        if self.host_url.is_none() {
            self.user_message = Some(UserMessage::Error(
                "Not connected to Ultimate64".to_string(),
            ));
            return Task::none();
        }

        let remote_dest = self.remote_browser.get_current_path().to_string();
        let existing: std::collections::HashSet<&str> = self
            .remote_browser
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        let conflicts: Vec<String> = items_to_copy
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .filter(|n| existing.contains(n))
            .map(String::from)
            .collect();

        self.pending_copy = Some(PendingCopy {
            items: items_to_copy,
            remote_dest,
            conflicts: conflicts.clone(),
        });
        if conflicts.is_empty() {
            return Task::done(Message::CopyOverwriteConfirm);
        }
        Task::none()
    }

    pub(crate) fn handle_copy_overwrite_cancel(&mut self) -> Task<Message> {
        self.pending_copy = None;
        Task::none()
    }

    pub(crate) fn handle_copy_overwrite_confirm(&mut self) -> Task<Message> {
        let pending = match self.pending_copy.take() {
            Some(p) => p,
            None => return Task::none(),
        };
        let items_to_copy = pending.items;
        let remote_dest = pending.remote_dest;

        let host = match &self.host_url {
            Some(h) => h
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .to_string(),
            None => {
                self.user_message = Some(UserMessage::Error(
                    "Not connected to Ultimate64".to_string(),
                ));
                return Task::none();
            }
        };

        let password = self.settings.connection.password.clone();
        let progress = self.copy_progress.clone();

        // Count total files and bytes (recursively walking directories)
        let mut total_files: usize = 0;
        let mut total_bytes: u64 = 0;
        for p in &items_to_copy {
            if p.is_dir() {
                for e in walkdir::WalkDir::new(p)
                    .min_depth(1)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if e.file_type().is_file() {
                        total_files += 1;
                        total_bytes =
                            total_bytes.saturating_add(e.metadata().map(|m| m.len()).unwrap_or(0));
                    }
                }
            } else {
                total_files += 1;
                total_bytes =
                    total_bytes.saturating_add(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
            }
        }

        if let Ok(mut g) = progress.lock() {
            *g = Some(crate::ftp_ops::TransferProgress {
                current: 0,
                total: total_files,
                current_file: String::new(),
                operation: "Uploading".to_string(),
                done: false,
                cancelled: false,
                started_at: std::time::Instant::now(),
                bytes_transferred: 0,
                bytes_total: total_bytes,
            });
        }

        self.user_message = Some(UserMessage::Info(format!(
            "Uploading {} file(s) via FTP...",
            total_files
        )));

        return Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    use std::io::Cursor;
                    use std::time::Duration;
                    use suppaftp::FtpStream;

                    let addr = format!("{}:21", host);
                    let mut ftp = FtpStream::connect(&addr)
                        .map_err(|e| format!("FTP connect failed: {}", e))?;

                    ftp.get_ref()
                        .set_write_timeout(Some(Duration::from_secs(120)))
                        .ok();

                    if let Some(ref pwd) = password {
                        if !pwd.is_empty() {
                            ftp.login("admin", pwd)
                                .map_err(|e| format!("FTP login failed: {}", e))?;
                        } else {
                            ftp.login("anonymous", "anonymous")
                                .map_err(|e| format!("FTP login failed: {}", e))?;
                        }
                    } else {
                        ftp.login("anonymous", "anonymous")
                            .map_err(|e| format!("FTP login failed: {}", e))?;
                    }

                    ftp.transfer_type(suppaftp::types::FileType::Binary)
                        .map_err(|e| format!("Set binary mode failed: {}", e))?;

                    let mut uploaded = 0usize;
                    let mut errors: Vec<String> = Vec::new();

                    for item_path in &items_to_copy {
                        // Check for cancellation
                        let is_cancelled = progress
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|p| p.cancelled))
                            .unwrap_or(false);
                        if is_cancelled {
                            break;
                        }

                        if item_path.is_dir() {
                            // Upload directory recursively
                            let dir_name = item_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("dir");
                            let base_remote =
                                format!("{}/{}", remote_dest.trim_end_matches('/'), dir_name);

                            for entry in walkdir::WalkDir::new(item_path).min_depth(0) {
                                // Check for cancellation inside dir walk
                                let is_cancelled = progress
                                    .lock()
                                    .ok()
                                    .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                    .unwrap_or(false);
                                if is_cancelled {
                                    break;
                                }
                                let entry = match entry {
                                    Ok(e) => e,
                                    Err(e) => {
                                        errors.push(format!("Walk error: {}", e));
                                        continue;
                                    }
                                };
                                let relative = match entry.path().strip_prefix(item_path) {
                                    Ok(r) => r,
                                    Err(_) => continue,
                                };
                                let remote_path = if relative.as_os_str().is_empty() {
                                    base_remote.clone()
                                } else {
                                    let rel_str = relative.to_string_lossy().replace('\\', "/");
                                    format!("{}/{}", base_remote, rel_str)
                                };

                                if entry.file_type().is_dir() {
                                    let _ = ftp.mkdir(&remote_path);
                                } else if entry.file_type().is_file() {
                                    let filename = entry
                                        .path()
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();

                                    if let Ok(mut g) = progress.lock() {
                                        if let Some(ref mut p) = *g {
                                            p.current_file = filename.clone();
                                        }
                                    }

                                    match std::fs::read(entry.path()) {
                                        Ok(data) => {
                                            let (parent_dir, fname) =
                                                if let Some(pos) = remote_path.rfind('/') {
                                                    (&remote_path[..pos], &remote_path[pos + 1..])
                                                } else {
                                                    ("/", remote_path.as_str())
                                                };
                                            if ftp.cwd(parent_dir).is_err() {
                                                errors.push(format!("CWD {}: failed", parent_dir));
                                                continue;
                                            }
                                            let cursor = Cursor::new(data);
                                            let mut reader = crate::ftp_ops::ProgressReader {
                                                inner: cursor,
                                                progress: progress.clone(),
                                            };
                                            match ftp.put_file(fname, &mut reader) {
                                                Ok(_) => {
                                                    uploaded += 1;
                                                    if let Ok(mut g) = progress.lock() {
                                                        if let Some(ref mut p) = *g {
                                                            p.current = uploaded;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    errors.push(format!("Upload {}: {}", fname, e))
                                                }
                                            }
                                        }
                                        Err(e) => errors.push(format!(
                                            "Read {}: {}",
                                            entry.path().display(),
                                            e
                                        )),
                                    }
                                }
                            }
                        } else {
                            // Upload single file
                            let filename = item_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("file")
                                .to_string();

                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current_file = filename.clone();
                                }
                            }

                            // CWD to remote dest for loose files
                            ftp.cwd(&remote_dest)
                                .map_err(|e| format!("Cannot access {}: {}", remote_dest, e))?;

                            let data = std::fs::read(item_path).map_err(|e| {
                                format!("Cannot read {}: {}", item_path.display(), e)
                            })?;
                            let cursor = Cursor::new(data);
                            let mut reader = crate::ftp_ops::ProgressReader {
                                inner: cursor,
                                progress: progress.clone(),
                            };
                            ftp.put_file(&filename, &mut reader)
                                .map_err(|e| format!("FTP upload {} failed: {}", filename, e))?;

                            uploaded += 1;
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current = uploaded;
                                }
                            }
                        }
                    }

                    let was_cancelled = progress
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|p| p.cancelled))
                        .unwrap_or(false);

                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.done = true;
                        }
                    }

                    let _ = ftp.quit();

                    let mut msg = if was_cancelled {
                        format!("Cancelled after {} file(s)", uploaded)
                    } else {
                        format!("Uploaded {} file(s)", uploaded)
                    };
                    if !errors.is_empty() {
                        msg.push_str(&format!(" ({} errors)", errors.len()));
                        for e in errors.iter().take(3) {
                            log::warn!("Upload error: {}", e);
                        }
                    }
                    Ok(msg)
                })
                .await
                .map_err(|e| e.to_string())?
            },
            Message::CopyComplete,
        );
    }

    pub(crate) fn handle_copy_remote_to_local(&mut self) -> Task<Message> {
        let (file_paths, dir_paths) = self.remote_browser.get_checked_files_and_dirs();

        // Fall back to single selected file if nothing checked
        let (file_paths, dir_paths) = if file_paths.is_empty() && dir_paths.is_empty() {
            if let Some(path) = self.remote_browser.get_selected_file() {
                (vec![path.to_string()], vec![])
            } else {
                self.user_message = Some(UserMessage::Error(
                    "No files selected. Use checkboxes to select files.".to_string(),
                ));
                return Task::none();
            }
        } else {
            (file_paths, dir_paths)
        };

        let host = match &self.host_url {
            Some(h) => h
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .to_string(),
            None => {
                self.user_message = Some(UserMessage::Error(
                    "Not connected to Ultimate64".to_string(),
                ));
                return Task::none();
            }
        };

        let local_dest = self.left_browser.get_current_directory().clone();
        let password = self.settings.connection.password.clone();
        let progress = self.copy_progress.clone();

        // Initial total is just file count; directories will be counted via FTP LIST
        if let Ok(mut g) = progress.lock() {
            *g = Some(crate::ftp_ops::TransferProgress {
                current: 0,
                total: file_paths.len(),
                current_file: "counting files...".to_string(),
                operation: "Downloading".to_string(),
                done: false,
                cancelled: false,
                started_at: std::time::Instant::now(),
                bytes_transferred: 0,
                bytes_total: 0,
            });
        }

        self.user_message = Some(UserMessage::Info("Downloading via FTP...".to_string()));

        return Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    use std::io::Read;
                    use std::time::Duration;
                    use suppaftp::FtpStream;

                    let addr = format!("{}:21", host);
                    let mut ftp = FtpStream::connect(&addr)
                        .map_err(|e| format!("FTP connect failed: {}", e))?;

                    ftp.get_ref()
                        .set_read_timeout(Some(Duration::from_secs(60)))
                        .ok();

                    if let Some(ref pwd) = password {
                        if !pwd.is_empty() {
                            ftp.login("admin", pwd)
                                .map_err(|e| format!("FTP login failed: {}", e))?;
                        } else {
                            ftp.login("anonymous", "anonymous")
                                .map_err(|e| format!("FTP login failed: {}", e))?;
                        }
                    } else {
                        ftp.login("anonymous", "anonymous")
                            .map_err(|e| format!("FTP login failed: {}", e))?;
                    }

                    ftp.transfer_type(suppaftp::types::FileType::Binary)
                        .map_err(|e| format!("Set binary mode failed: {}", e))?;

                    let mut downloaded = 0usize;
                    let mut errors: Vec<String> = Vec::new();

                    // Count total files in directories via FTP LIST
                    let mut dir_file_count = 0usize;
                    for remote_dir in &dir_paths {
                        dir_file_count += count_remote_files_recursive(&mut ftp, remote_dir);
                    }
                    // Update total with actual file count
                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.total = file_paths.len() + dir_file_count;
                            p.current_file = String::new();
                        }
                    }

                    // Download individual files
                    for remote_path in &file_paths {
                        let is_cancelled = progress
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|p| p.cancelled))
                            .unwrap_or(false);
                        if is_cancelled {
                            break;
                        }

                        let filename = remote_path.rsplit('/').next().unwrap_or("file");

                        // Get file size for progress
                        let file_size = ftp.size(remote_path).unwrap_or(0);

                        if let Ok(mut g) = progress.lock() {
                            if let Some(ref mut p) = *g {
                                p.current_file = filename.to_string();
                                p.bytes_total += file_size as u64;
                            }
                        }

                        match ftp.retr_as_stream(remote_path) {
                            Ok(mut reader) => {
                                let mut data = Vec::new();
                                if let Err(e) = reader.read_to_end(&mut data) {
                                    errors.push(format!("{}: {}", filename, e));
                                    continue;
                                }
                                if let Err(e) = ftp.finalize_retr_stream(reader) {
                                    errors.push(format!("{}: {}", filename, e));
                                    continue;
                                }
                                let local_path = local_dest.join(filename);
                                if let Err(e) = std::fs::write(&local_path, &data) {
                                    errors.push(format!("{}: {}", filename, e));
                                    continue;
                                }
                                downloaded += 1;
                                if let Ok(mut g) = progress.lock() {
                                    if let Some(ref mut p) = *g {
                                        p.current = downloaded;
                                        p.bytes_transferred += data.len() as u64;
                                    }
                                }
                            }
                            Err(e) => {
                                errors.push(format!("{}: {}", filename, e));
                            }
                        }
                    }

                    // Download directories recursively
                    for remote_dir in &dir_paths {
                        let is_cancelled = progress
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|p| p.cancelled))
                            .unwrap_or(false);
                        if is_cancelled {
                            break;
                        }

                        let dir_name = remote_dir.rsplit('/').next().unwrap_or("dir");
                        let local_dir = local_dest.join(dir_name);

                        if let Ok(mut g) = progress.lock() {
                            if let Some(ref mut p) = *g {
                                p.current_file = format!("{}/", dir_name);
                            }
                        }

                        match download_directory_with_progress(
                            &mut ftp,
                            remote_dir,
                            &local_dir,
                            &progress,
                            &mut downloaded,
                        ) {
                            Ok(files) => {
                                log::info!("Downloaded dir {}: {} files", dir_name, files);
                            }
                            Err(e) => {
                                errors.push(format!("{}: {}", dir_name, e));
                            }
                        }
                    }

                    let was_cancelled = progress
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|p| p.cancelled))
                        .unwrap_or(false);

                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.done = true;
                        }
                    }

                    let _ = ftp.quit();

                    let mut msg = if was_cancelled {
                        format!("Cancelled after {} item(s)", downloaded)
                    } else {
                        format!("Downloaded {} item(s)", downloaded)
                    };
                    if !errors.is_empty() {
                        msg.push_str(&format!(" ({} errors)", errors.len()));
                        for e in errors.iter().take(3) {
                            log::warn!("Download error: {}", e);
                        }
                    }
                    Ok(msg)
                })
                .await
                .map_err(|e| e.to_string())?
            },
            Message::CopyComplete,
        );
    }

    pub(crate) fn handle_copy_cancel(&mut self) -> Task<Message> {
        if let Ok(mut g) = self.copy_progress.lock() {
            if let Some(ref mut p) = *g {
                p.cancelled = true;
            }
        }
        Task::none()
    }

    pub(crate) fn handle_copy_progress_tick(&mut self) -> Task<Message> {
        // Just triggers a re-render so the progress bar updates
        Task::none()
    }

    pub(crate) fn handle_copy_complete(
        &mut self,
        result: Result<String, String>,
        ctx: TabContext,
    ) -> Task<Message> {
        // Clear copy progress
        if let Ok(mut g) = self.copy_progress.lock() {
            *g = None;
        }
        match result {
            Ok(msg) => {
                self.show_toast(msg.clone());
                self.user_message = Some(UserMessage::Info(msg));
                // Clear checked files after successful copy
                self.left_browser.clear_checked();
                // Refresh both browsers
                return Task::batch(vec![
                    self.left_browser
                        .update(FileBrowserMessage::RefreshFiles, ctx.clone())
                        .map(Message::LeftBrowser),
                    self.remote_browser
                        .update(RemoteBrowserMessage::RefreshFiles, ctx.clone())
                        .map(Message::RemoteBrowser),
                ]);
            }
            Err(e) => {
                self.user_message = Some(UserMessage::Error(e));
            }
        }
        Task::none()
    }
}
