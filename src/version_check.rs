use iced::Command;
use serde::Deserialize;

/// GitHub release info
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: String,
}

/// Version check result
#[derive(Debug, Clone)]
pub struct NewVersionInfo {
    pub version: String,
    pub download_url: String,
}

/// Message type for version checking
#[derive(Debug, Clone)]
pub enum VersionCheckMessage {
    CheckComplete(Result<Option<NewVersionInfo>, String>),
}

/// Check GitHub for new version
pub fn check_for_updates(current_version: &str) -> Command<VersionCheckMessage> {
    let current = current_version.to_string();

    Command::perform(
        async move { check_github_release(&current).await },
        VersionCheckMessage::CheckComplete,
    )
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
        Ok(Some(NewVersionInfo {
            version: release.tag_name,
            download_url: release.html_url,
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
}
