use iced::Task;
use serde::Deserialize;

/// GitHub release asset info
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// GitHub release info
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

/// Version check result
#[derive(Debug, Clone)]
pub struct NewVersionInfo {
    pub version: String,
    /// Direct download URL for the platform-specific binary, falls back to release page
    pub download_url: String,
}

/// Message type for version checking
#[derive(Debug, Clone)]
pub enum VersionCheckMessage {
    CheckComplete(Result<Option<NewVersionInfo>, String>),
}

/// Check GitHub for new version
pub fn check_for_updates(current_version: &str) -> Task<VersionCheckMessage> {
    let current = current_version.to_string();
    Task::perform(
        async move { check_github_release(&current).await },
        VersionCheckMessage::CheckComplete,
    )
}

/// Find the download URL for the current platform from release assets
fn find_platform_asset(assets: &[GitHubAsset]) -> Option<&str> {
    let target = if cfg!(target_os = "windows") {
        "Win.exe"
    } else if cfg!(target_os = "macos") {
        "MacOS.zip"
    } else if cfg!(target_os = "linux") {
        "Linux.AppImage"
    } else {
        return None;
    };

    assets
        .iter()
        .find(|a| a.name.ends_with(target))
        .map(|a| a.browser_download_url.as_str())
}

async fn check_github_release(current_version: &str) -> Result<Option<NewVersionInfo>, String> {
    let client = reqwest::Client::builder()
        .user_agent("Ultimate64-Manager")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Client error: {}", e))?;

    let response = client
        .get("https://api.github.com/repos/sandlbn/ultimate64-manager/releases/latest")
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API error: {}", response.status()));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("Parse error: {}", e))?;

    // Remove 'v' prefix if present for comparison
    let latest = release.tag_name.trim_start_matches('v');
    let current = current_version.trim_start_matches('v');

    if is_newer_version(latest, current) {
        // Use platform-specific asset URL if available, otherwise fall back to release page
        let download_url = find_platform_asset(&release.assets)
            .map(|s| s.to_string())
            .unwrap_or(release.html_url);

        Ok(Some(NewVersionInfo {
            version: release.tag_name,
            download_url,
        }))
    } else {
        Ok(None)
    }
}

/// Compare semantic versions (e.g., "0.3.4" > "0.3.3")
fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_version =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse().ok()).collect() };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for i in 0..latest_parts.len().max(current_parts.len()) {
        let l = latest_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if l > c {
            return true;
        } else if l < c {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        assert!(is_newer_version("0.3.4", "0.3.3"));
        assert!(is_newer_version("0.4.0", "0.3.9"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.3.3", "0.3.3"));
        assert!(!is_newer_version("0.3.2", "0.3.3"));
    }

    #[test]
    fn test_find_platform_asset() {
        let assets = vec![
            GitHubAsset {
                name: "Ultimate64Manager-Linux.AppImage".to_string(),
                browser_download_url: "https://github.com/sandlbn/ultimate64-manager/releases/download/v0.3.12/Ultimate64Manager-Linux.AppImage".to_string(),
            },
            GitHubAsset {
                name: "Ultimate64Manager-MacOS.zip".to_string(),
                browser_download_url: "https://github.com/sandlbn/ultimate64-manager/releases/download/v0.3.12/Ultimate64Manager-MacOS.zip".to_string(),
            },
            GitHubAsset {
                name: "Ultimate64Manager-Win.exe".to_string(),
                browser_download_url: "https://github.com/sandlbn/ultimate64-manager/releases/download/v0.3.12/Ultimate64Manager-Win.exe".to_string(),
            },
        ];

        let result = find_platform_asset(&assets);
        assert!(result.is_some());
        // The exact URL depends on the platform running the test
    }

    #[test]
    fn test_find_platform_asset_empty() {
        let assets: Vec<GitHubAsset> = vec![];
        assert!(find_platform_asset(&assets).is_none());
    }
}
