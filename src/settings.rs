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
                host: String::new(), // Empty by default - user must configure
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
    fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-manager");

        fs::create_dir_all(&config_dir)?;
        Ok(config_dir.join("settings.json"))
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_file = Self::config_path()?;

        log::debug!("Loading settings from: {:?}", config_file);

        if config_file.exists() {
            let contents = fs::read_to_string(&config_file)?;
            let settings: AppSettings = serde_json::from_str(&contents)?;
            log::info!("Settings loaded successfully");
            Ok(settings)
        } else {
            log::info!("No settings file found, using defaults");
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_file = Self::config_path()?;

        log::debug!("Saving settings to: {:?}", config_file);

        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&config_file, contents)?;

        log::info!("Settings saved successfully");
        Ok(())
    }
}
