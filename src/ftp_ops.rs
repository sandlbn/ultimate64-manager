use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

use crate::dir_preview::ContentPreview;
use crate::disk_image::{self, DiskInfo};

/// Timeout for FTP operations to prevent hangs when device goes offline
pub const FTP_TIMEOUT_SECS: u64 = 15;
/// Longer timeout for directory uploads which may take time
pub const FTP_UPLOAD_DIR_TIMEOUT_SECS: u64 = 120;
/// Longer timeout for content preview downloads (PDFs can be large)
pub const FTP_PREVIEW_TIMEOUT_SECS: u64 = 60;

/// Shared progress state between async FTP tasks and the UI.
/// Updated by blocking tasks, polled by iced subscription every 250ms.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub current: usize,
    pub total: usize,
    pub current_file: String,
    pub operation: String, // "Downloading", "Uploading", "Deleting", etc.
    pub done: bool,
}

/// Disk format chosen in the create-disk dialog
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiskCreateType {
    D64,
    D71,
    D81,
}

impl std::fmt::Display for DiskCreateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiskCreateType::D64 => write!(f, "D64  (1541 · 174 KB)"),
            DiskCreateType::D71 => write!(f, "D71  (1571 · 349 KB)"),
            DiskCreateType::D81 => write!(f, "D81  (1581 · 800 KB)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub path: String,
}

// ─── Delete via FTP ───────────────────────────────────────────────────────────

/// Delete a list of remote paths. Directories are deleted recursively.
/// `paths_with_type` is `(remote_path, is_dir)`.
pub async fn delete_ftp(
    host: String,
    paths_with_type: Vec<(String, bool)>,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    let total = paths_with_type.len();
    log::info!("FTP: Deleting {} item(s)", total);

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_UPLOAD_DIR_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(30)))
                .ok();
            ftp.get_ref()
                .set_write_timeout(Some(Duration::from_secs(30)))
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

            let mut deleted = 0usize;
            let mut errors: Vec<String> = Vec::new();

            for (i, (path, is_dir)) in paths_with_type.iter().enumerate() {
                let name = path.rsplit('/').next().unwrap_or(path).to_string();

                // Update progress
                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current = i;
                        p.current_file = name.clone();
                    }
                }

                if *is_dir {
                    match delete_dir_recursive_ftp(&mut ftp, path) {
                        Ok(count) => deleted += count,
                        Err(e) => errors.push(format!("{}: {}", name, e)),
                    }
                } else {
                    match ftp.rm(path) {
                        Ok(_) => {
                            deleted += 1;
                            log::debug!("FTP: Deleted file {}", path);
                        }
                        Err(e) => errors.push(format!("{}: {}", name, e)),
                    }
                }
            }

            // Mark progress done
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current = total;
                    p.done = true;
                }
            }

            let _ = ftp.quit();

            let mut msg = format!("Deleted {} item(s)", deleted);
            if !errors.is_empty() {
                msg.push_str(&format!(" ({} errors)", errors.len()));
                for e in errors.iter().take(3) {
                    log::warn!("Delete error: {}", e);
                }
            }
            Ok(msg)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Delete timed out".to_string()),
    }
}

/// Recursively delete a remote directory and all its contents via FTP.
/// Returns the number of items deleted.
pub fn delete_dir_recursive_ftp(
    ftp: &mut suppaftp::FtpStream,
    remote_path: &str,
) -> Result<usize, String> {
    let mut deleted = 0usize;

    // List contents
    let entries = ftp
        .list(Some(remote_path))
        .map_err(|e| format!("List {}: {}", remote_path, e))?;

    for line in &entries {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name = parts[8..].join(" ");
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = line.starts_with('d');
        let child = format!("{}/{}", remote_path.trim_end_matches('/'), name);

        if is_dir {
            deleted += delete_dir_recursive_ftp(ftp, &child)?;
        } else {
            match ftp.rm(&child) {
                Ok(_) => {
                    deleted += 1;
                    log::debug!("FTP: Deleted {}", child);
                }
                Err(e) => log::warn!("FTP: Failed to delete {}: {}", child, e),
            }
        }
    }

    // Now remove the (now-empty) directory itself
    match ftp.rmdir(remote_path) {
        Ok(_) => {
            deleted += 1;
            log::debug!("FTP: Removed directory {}", remote_path);
        }
        Err(e) => log::warn!("FTP: Failed to rmdir {}: {}", remote_path, e),
    }

    Ok(deleted)
}

// ─── Rename via FTP ───────────────────────────────────────────────────────────

/// Rename/move a remote file or directory using FTP RNFR/RNTO.
pub async fn rename_ftp(
    host: String,
    old_path: String,
    new_path: String,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("FTP: Renaming {} → {}", old_path, new_path);

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(15)))
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

            let new_name = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();

            ftp.rename(&old_path, &new_path)
                .map_err(|e| format!("Rename failed: {}", e))?;

            let _ = ftp.quit();

            Ok(format!("Renamed to {}", new_name))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Rename timed out".to_string()),
    }
}

pub async fn fetch_files_ftp(
    host: String,
    path: String,
    password: Option<String>,
) -> Result<Vec<RemoteFileEntry>, String> {
    log::info!("FTP: Listing {} on {}", path, host);

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;

            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(10)))
                .ok();
            ftp.get_ref()
                .set_write_timeout(Some(Duration::from_secs(10)))
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

            let ftp_path = if path.is_empty() || path == "/" {
                "/"
            } else {
                &path
            };
            ftp.cwd(ftp_path)
                .map_err(|e| format!("Cannot access {}: {}", ftp_path, e))?;

            let list = ftp
                .list(None)
                .map_err(|e| format!("FTP list failed: {}", e))?;

            let mut entries = Vec::new();
            for line in list {
                if let Some(entry) = parse_ftp_line(&line, &path) {
                    if entry.name != "." && entry.name != ".." {
                        entries.push(entry);
                    }
                }
            }

            entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            let _ = ftp.quit();
            Ok(entries)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP list timed out - device may be offline".to_string()),
    }
}

pub fn parse_ftp_line(line: &str, parent_path: &str) -> Option<RemoteFileEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    if line.len() > 10 && (line.starts_with('d') || line.starts_with('-') || line.starts_with('l'))
    {
        let is_dir = line.starts_with('d');
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 9 {
            let size: u64 = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let name = parts[8..].join(" ");
            if name.is_empty() || name == "." || name == ".." {
                return None;
            }
            let entry_path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path.trim_end_matches('/'), name)
            };
            return Some(RemoteFileEntry {
                name,
                is_dir,
                size,
                path: entry_path,
            });
        }
    }

    if line.contains("<DIR>") {
        let parts: Vec<&str> = line.split("<DIR>").collect();
        if parts.len() == 2 {
            let name = parts[1].trim().to_string();
            if name.is_empty() || name == "." || name == ".." {
                return None;
            }
            let entry_path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path.trim_end_matches('/'), name)
            };
            return Some(RemoteFileEntry {
                name,
                is_dir: true,
                size: 0,
                path: entry_path,
            });
        }
    }

    let parts: Vec<&str> = line.split_whitespace().collect();
    if !parts.is_empty() {
        let name = parts[0].to_string();
        let is_dir = name.ends_with('/');
        let name = name.trim_end_matches('/').to_string();
        if name.is_empty() || name == "." || name == ".." {
            return None;
        }
        let size: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let entry_path = if parent_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent_path.trim_end_matches('/'), name)
        };
        return Some(RemoteFileEntry {
            name,
            is_dir,
            size,
            path: entry_path,
        });
    }

    None
}

pub async fn download_file_ftp_preview(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<(String, Vec<u8>), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_PREVIEW_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(120)))
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
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let filename = remote_path.rsplit('/').next().unwrap_or("file").to_string();
            let mut reader = ftp
                .retr_as_stream(&remote_path)
                .map_err(|e| format!("FTP download failed: {}", e))?;
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .map_err(|e| format!("Read error: {}", e))?;
            ftp.finalize_retr_stream(reader)
                .map_err(|e| format!("Transfer finalize error: {}", e))?;
            let _ = ftp.quit();
            Ok((filename, data))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Download timed out - file may be too large".to_string()),
    }
}

pub async fn download_file_ftp_with_progress(
    host: String,
    remote_path: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<(String, Vec<u8>), String> {
    let result = download_file_ftp(host, remote_path, password).await;
    if let Ok(mut g) = progress.lock() {
        if let Some(ref mut p) = *g {
            p.current = 1;
            p.done = true;
        }
    }
    result
}

pub async fn download_file_ftp(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<(String, Vec<u8>), String> {
    log::info!("FTP: Downloading {}", remote_path);

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
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
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let filename = remote_path.rsplit('/').next().unwrap_or("file").to_string();
            let mut reader = ftp
                .retr_as_stream(&remote_path)
                .map_err(|e| format!("FTP download failed: {}", e))?;
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .map_err(|e| format!("Read error: {}", e))?;
            ftp.finalize_retr_stream(reader)
                .map_err(|e| format!("Transfer finalize error: {}", e))?;
            let _ = ftp.quit();
            Ok((filename, data))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP download timed out - device may be offline".to_string()),
    }
}

pub async fn upload_file_ftp(
    host: String,
    local_path: PathBuf,
    remote_dest: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Cursor;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let data =
                std::fs::read(&local_path).map_err(|e| format!("Cannot read file: {}", e))?;
            let filename = local_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
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
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let dest_dir = if remote_dest.ends_with('/') {
                remote_dest.as_str()
            } else {
                remote_dest.rsplit_once('/').map(|(d, _)| d).unwrap_or("/")
            };
            ftp.cwd(dest_dir)
                .map_err(|e| format!("Cannot access {}: {}", dest_dir, e))?;

            let mut cursor = Cursor::new(data);
            ftp.put_file(&filename, &mut cursor)
                .map_err(|e| format!("FTP upload failed: {}", e))?;
            let _ = ftp.quit();

            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current = 1;
                    p.done = true;
                }
            }
            Ok(filename)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP upload timed out - device may be offline".to_string()),
    }
}

pub async fn upload_directory_ftp(
    host: String,
    local_path: PathBuf,
    remote_dest: String,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_UPLOAD_DIR_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Cursor;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let total_files = WalkDir::new(&local_path)
                .min_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .count();

            if let Ok(mut g) = progress.lock() {
                *g = Some(TransferProgress {
                    current: 0,
                    total: total_files,
                    current_file: String::new(),
                    operation: "Uploading".to_string(),
                    done: false,
                });
            }

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
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
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let dir_name = local_path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| "Invalid directory name".to_string())?;

            let base_remote = if remote_dest.ends_with('/') {
                format!("{}{}", remote_dest, dir_name)
            } else {
                format!("{}/{}", remote_dest, dir_name)
            };

            let mut dirs_created = 0;
            let mut files_uploaded = 0;
            let mut errors: Vec<String> = Vec::new();

            for entry in WalkDir::new(&local_path).min_depth(0) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        errors.push(format!("Walk error: {}", e));
                        continue;
                    }
                };

                let relative = match entry.path().strip_prefix(&local_path) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let remote_path = if relative.as_os_str().is_empty() {
                    base_remote.clone()
                } else {
                    let relative_str = relative.to_string_lossy().replace('\\', "/");
                    format!("{}/{}", base_remote, relative_str)
                };

                if entry.file_type().is_dir() {
                    match ftp.mkdir(&remote_path) {
                        Ok(_) => {
                            dirs_created += 1;
                        }
                        Err(e) => {
                            log::debug!("FTP: mkdir {} (may exist): {}", remote_path, e);
                        }
                    }
                } else if entry.file_type().is_file() {
                    let filename_display = entry
                        .path()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if let Ok(mut g) = progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.current_file = filename_display;
                        }
                    }

                    let data = match std::fs::read(entry.path()) {
                        Ok(d) => d,
                        Err(e) => {
                            errors.push(format!("Read {}: {}", entry.path().display(), e));
                            continue;
                        }
                    };

                    let (parent_dir, filename) = if let Some(pos) = remote_path.rfind('/') {
                        (&remote_path[..pos], &remote_path[pos + 1..])
                    } else {
                        ("/", remote_path.as_str())
                    };

                    if let Err(e) = ftp.cwd(parent_dir) {
                        errors.push(format!("CWD {}: {}", parent_dir, e));
                        continue;
                    }

                    let mut cursor = Cursor::new(data);
                    match ftp.put_file(filename, &mut cursor) {
                        Ok(_) => {
                            files_uploaded += 1;
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current = files_uploaded;
                                }
                            }
                        }
                        Err(e) => {
                            errors.push(format!("Upload {}: {}", filename, e));
                        }
                    }
                }
            }

            let _ = ftp.quit();

            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.done = true;
                }
            }

            let mut msg = format!(
                "Uploaded: {} files, {} directories",
                files_uploaded, dirs_created
            );
            if !errors.is_empty() {
                msg.push_str(&format!(" ({} errors)", errors.len()));
            }
            Ok(msg)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP directory upload timed out - device may be offline".to_string()),
    }
}

pub async fn download_batch_ftp(
    host: String,
    file_paths: Vec<String>,
    dir_paths: Vec<String>,
    local_dest: PathBuf,
    password: Option<String>,
    progress: Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<String, String> {
    let total = file_paths.len() + dir_paths.len();
    if let Ok(mut g) = progress.lock() {
        *g = Some(TransferProgress {
            current: 0,
            total,
            current_file: String::new(),
            operation: "Downloading".to_string(),
            done: false,
        });
    }

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_UPLOAD_DIR_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
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
                .map_err(|e| format!("Failed to set binary mode: {}", e))?;

            let mut files_downloaded = 0;
            let mut dirs_downloaded = 0;
            let mut items_completed = 0;
            let mut errors: Vec<String> = Vec::new();

            for remote_path in &file_paths {
                let filename = remote_path.rsplit('/').next().unwrap_or("file");
                let local_path = local_dest.join(filename);

                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current_file = filename.to_string();
                    }
                }

                match ftp.retr_as_stream(remote_path) {
                    Ok(mut reader) => {
                        let mut data = Vec::new();
                        if let Err(e) = reader.read_to_end(&mut data) {
                            errors.push(format!("Read {}: {}", filename, e));
                            continue;
                        }
                        if let Err(e) = ftp.finalize_retr_stream(reader) {
                            errors.push(format!("Finalize {}: {}", filename, e));
                            continue;
                        }
                        if let Err(e) = std::fs::write(&local_path, &data) {
                            errors.push(format!("Write {}: {}", filename, e));
                            continue;
                        }
                        files_downloaded += 1;
                    }
                    Err(e) => {
                        errors.push(format!("Download {}: {}", filename, e));
                    }
                }

                items_completed += 1;
                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current = items_completed;
                    }
                }
            }

            for remote_dir in &dir_paths {
                let dir_name = remote_dir.rsplit('/').next().unwrap_or("dir");
                let local_dir = local_dest.join(dir_name);

                if let Ok(mut g) = progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.current = 0;
                        p.total = 0;
                        p.current_file = format!("{}/", dir_name);
                        p.operation = "Downloading".to_string();
                    }
                }

                match download_directory_recursive(&mut ftp, remote_dir, &local_dir, &progress) {
                    Ok((files, dirs)) => {
                        files_downloaded += files;
                        dirs_downloaded += dirs;
                    }
                    Err(e) => {
                        errors.push(format!("Dir {}: {}", dir_name, e));
                    }
                }
            }

            let _ = ftp.quit();

            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.done = true;
                }
            }

            let mut msg = format!(
                "Downloaded: {} files, {} directories",
                files_downloaded, dirs_downloaded
            );
            if !errors.is_empty() {
                msg.push_str(&format!(" ({} errors)", errors.len()));
            }
            Ok(msg)
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("FTP batch download timed out - device may be offline".to_string()),
    }
}

pub fn download_directory_recursive(
    ftp: &mut suppaftp::FtpStream,
    remote_path: &str,
    local_path: &std::path::Path,
    progress: &Arc<std::sync::Mutex<Option<TransferProgress>>>,
) -> Result<(usize, usize), String> {
    use std::io::Read;

    std::fs::create_dir_all(local_path)
        .map_err(|e| format!("Create dir {}: {}", local_path.display(), e))?;

    let mut files_count = 0;
    let mut dirs_count = 1;

    let entries = ftp
        .list(Some(remote_path))
        .map_err(|e| format!("List {}: {}", remote_path, e))?;

    for entry_line in &entries {
        let parts: Vec<&str> = entry_line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name = parts[8..].join(" ");
        if name == "." || name == ".." {
            continue;
        }

        let is_dir = entry_line.starts_with('d');
        let child_remote = format!("{}/{}", remote_path.trim_end_matches('/'), name);
        let child_local = local_path.join(&name);

        if is_dir {
            match download_directory_recursive(ftp, &child_remote, &child_local, progress) {
                Ok((f, d)) => {
                    files_count += f;
                    dirs_count += d;
                }
                Err(e) => {
                    log::warn!("Skip dir {}: {}", child_remote, e);
                }
            }
        } else {
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current_file = name.clone();
                }
            }
            match ftp.retr_as_stream(&child_remote) {
                Ok(mut reader) => {
                    let mut data = Vec::new();
                    if reader.read_to_end(&mut data).is_ok() {
                        let _ = ftp.finalize_retr_stream(reader);
                        if std::fs::write(&child_local, &data).is_ok() {
                            files_count += 1;
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current += 1;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Skip file {}: {}", child_remote, e);
                }
            }
        }
    }

    Ok((files_count, dirs_count))
}

pub async fn load_remote_disk_info(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<DiskInfo, String> {
    let (_, data) = download_file_ftp(host, remote_path, password).await?;
    tokio::task::spawn_blocking(move || disk_image::read_disk_info_from_bytes(&data))
        .await
        .map_err(|e| format!("Task error: {}", e))?
}

pub async fn load_remote_content_preview(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<ContentPreview, String> {
    let filename = remote_path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();
    let (_, data) = download_file_ftp_preview(host, remote_path.clone(), password).await?;

    if crate::file_types::is_text_file(&filename) {
        tokio::task::spawn_blocking(move || {
            let lower = filename.to_lowercase();
            let content = if lower.ends_with(".atxt") {
                crate::petscii::convert_text_file(&data)
            } else {
                match String::from_utf8(data.clone()) {
                    Ok(s) => s,
                    Err(_) => String::from_utf8_lossy(&data).to_string(),
                }
            };
            let line_count = content.lines().count();
            Ok(ContentPreview::Text {
                filename,
                content,
                line_count,
            })
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    } else if crate::file_types::is_image_file(&filename) {
        tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&data)
                .map_err(|e| format!("Failed to decode image: {}", e))?;
            let width = img.width();
            let height = img.height();
            Ok(ContentPreview::Image {
                filename,
                data,
                width,
                height,
            })
        })
        .await
        .map_err(|e| format!("Task error: {}", e))?
    } else if crate::file_types::is_pdf_file(&filename) {
        crate::pdf_preview::load_pdf_preview_from_bytes_async(data, filename).await
    } else {
        Err("Unsupported file type for preview".to_string())
    }
}

pub fn create_and_upload_disk(
    host: String,
    name: String,
    disk_id: String,
    disk_type: DiskCreateType,
    remote_dest: String,
    password: Option<String>,
) -> Result<String, String> {
    use std::io::Cursor;
    use std::time::Duration;
    use suppaftp::FtpStream;

    let safe_name = name.replace(' ', "_");
    let (ext, data) = match disk_type {
        DiskCreateType::D64 => ("d64", disk_image::build_blank_d64(&name, &disk_id)),
        DiskCreateType::D71 => ("d71", disk_image::build_blank_d71(&name, &disk_id)),
        DiskCreateType::D81 => ("d81", disk_image::build_blank_d81(&name, &disk_id)),
    };
    let filename = format!("{}.{}", safe_name, ext);
    let remote_path = format!("{}/{}", remote_dest.trim_end_matches('/'), filename);

    let addr = format!("{}:21", host);
    let mut ftp = FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
    ftp.get_ref()
        .set_write_timeout(Some(Duration::from_secs(60)))
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
        .map_err(|e| format!("Failed to set binary mode: {}", e))?;

    let dest_dir = remote_dest.trim_end_matches('/');
    ftp.cwd(dest_dir)
        .map_err(|e| format!("Cannot cd to {}: {}", dest_dir, e))?;

    let mut cursor = Cursor::new(data);
    ftp.put_file(&filename, &mut cursor)
        .map_err(|e| format!("FTP upload failed: {}", e))?;

    ftp.quit().ok();
    Ok(remote_path)
}

/// Create a directory on the remote device via FTP.
pub async fn mkdir_ftp(
    host: String,
    remote_path: String,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("FTP: Creating directory {}", remote_path);

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(FTP_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            use std::time::Duration;
            use suppaftp::FtpStream;

            let addr = format!("{}:21", host);
            let mut ftp =
                FtpStream::connect(&addr).map_err(|e| format!("FTP connect failed: {}", e))?;
            ftp.get_ref()
                .set_read_timeout(Some(Duration::from_secs(15)))
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

            ftp.mkdir(&remote_path)
                .map_err(|e| format!("MkDir failed: {}", e))?;

            let _ = ftp.quit();
            let dir_name = remote_path.rsplit('/').next().unwrap_or(&remote_path);
            Ok(format!("Created directory: {}", dir_name))
        }),
    )
    .await;

    match result {
        Ok(Ok(inner)) => inner,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("MkDir timed out".to_string()),
    }
}
