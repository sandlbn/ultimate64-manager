use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub connection: ConnectionSettings,
    pub default_paths: DefaultPaths,
    pub preferences: Preferences,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSettings {
    pub host: String,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultPaths {
    pub disk_images: Option<PathBuf>,
    pub music_files: Option<PathBuf>,
    pub programs: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub auto_mount_and_run: bool,
    pub default_mount_mode: String,
    pub show_hidden_files: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            connection: ConnectionSettings {
                host: String::from("192.168.1.64"),
                password: None,
            },
            default_paths: DefaultPaths {
                disk_images: None,
                music_files: None,
                programs: None,
            },
            preferences: Preferences {
                auto_mount_and_run: false,
                default_mount_mode: String::from("readwrite"),
                show_hidden_files: false,
            },
        }
    }
}

impl AppSettings {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-browser");

        fs::create_dir_all(&config_dir)?;

        let config_file = config_dir.join("settings.json");

        if config_file.exists() {
            let contents = fs::read_to_string(config_file)?;
            Ok(serde_json::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-browser");

        fs::create_dir_all(&config_dir)?;

        let config_file = config_dir.join("settings.json");
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(config_file, contents)?;

        Ok(())
    }
}
