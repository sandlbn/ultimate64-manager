//! ZIP archive extraction utilities.
//!
//! Extracted from the old `csdb` module — these helpers are not source-specific
//! and are reused by the Assembly64 browser when a release ships its files
//! inside a ZIP. Path components inside the archive are stripped so every
//! file lands directly in the target directory; hidden and macOS metadata
//! entries are skipped.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Maximum total uncompressed size we'll extract (100 MB).
pub const MAX_ZIP_EXTRACT_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedZip {
    pub source_filename: String,
    pub extract_dir: PathBuf,
    pub files: Vec<ExtractedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFile {
    pub index: usize,
    pub filename: String,
    pub path: PathBuf,
    pub ext: String,
    pub size: u64,
}

/// Extract a ZIP from in-memory bytes into `target_dir`. Returns the file
/// listing. The directory is created if missing; existing files are
/// overwritten. Path components are flattened (every file lands directly
/// inside `target_dir`).
pub fn extract_zip_to_dir(
    zip_data: &[u8],
    source_filename: &str,
    target_dir: &Path,
) -> Result<ExtractedZip> {
    let reader = Cursor::new(zip_data);
    let mut archive = ZipArchive::new(reader).context("Failed to open ZIP archive")?;

    std::fs::create_dir_all(target_dir).context("Failed to create target directory")?;

    let mut files = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read ZIP entry")?;

        if file.is_dir() {
            continue;
        }

        let filename = file.name().to_string();

        if filename.ends_with('/')
            || filename.starts_with('.')
            || filename.contains("/__MACOSX")
            || filename.contains("\\_MACOSX")
        {
            continue;
        }

        let clean_filename = filename.rsplit('/').next().unwrap_or(&filename).to_string();
        if clean_filename.is_empty() || clean_filename.starts_with('.') {
            continue;
        }

        let out_path = target_dir.join(&clean_filename);

        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .context("Failed to read ZIP entry contents")?;

        let size = contents.len() as u64;

        std::fs::write(&out_path, &contents)
            .with_context(|| format!("Failed to write {:?}", out_path))?;

        let ext = ext_of(&clean_filename);

        files.push(ExtractedFile {
            index: files.len() + 1,
            filename: clean_filename,
            path: out_path,
            ext,
            size,
        });
    }

    for (i, f) in files.iter_mut().enumerate() {
        f.index = i + 1;
    }

    Ok(ExtractedZip {
        source_filename: source_filename.to_string(),
        extract_dir: target_dir.to_path_buf(),
        files,
    })
}

/// Filter to extracted files that the Ultimate64 can run/mount directly.
pub fn runnable_extracted_files(files: &[ExtractedFile]) -> Vec<&ExtractedFile> {
    files
        .iter()
        .filter(|f| {
            matches!(
                f.ext.as_str(),
                "prg" | "d64" | "d71" | "d81" | "g64" | "crt" | "sid"
            )
        })
        .collect()
}

/// Lower-case extension without the leading dot, or empty string.
pub fn ext_of(filename: &str) -> String {
    match filename.rfind('.') {
        Some(pos) => filename[pos + 1..].to_lowercase(),
        None => String::new(),
    }
}

/// Re-export of [`crate::file_types::is_zip_file`] for callers that already
/// import this module.
pub fn is_zip_file(ext: &str) -> bool {
    crate::file_types::is_zip_file(ext)
}
