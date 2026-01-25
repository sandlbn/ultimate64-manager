use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Stream control method for communicating with Ultimate64
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StreamControlMethod {
    /// Try REST API first, fall back to binary protocol (default)
    #[default]
    RestWithBinaryFallback,
    /// Try binary protocol first, fall back to REST API
    BinaryWithRestFallback,
    /// Use only REST API (no fallback)
    RestOnly,
    /// Use only binary protocol on port 64 (no fallback)
    BinaryOnly,
}

impl StreamControlMethod {
    pub const ALL: [StreamControlMethod; 4] = [
        StreamControlMethod::RestWithBinaryFallback,
        StreamControlMethod::BinaryWithRestFallback,
        StreamControlMethod::RestOnly,
        StreamControlMethod::BinaryOnly,
    ];
}

impl std::fmt::Display for StreamControlMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamControlMethod::RestWithBinaryFallback => write!(f, "REST API (binary fallback)"),
            StreamControlMethod::BinaryWithRestFallback => write!(f, "Binary (REST fallback)"),
            StreamControlMethod::RestOnly => write!(f, "REST API only"),
            StreamControlMethod::BinaryOnly => write!(f, "Binary protocol only"),
        }
    }
}

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
    /// Stream control method for video/audio streaming
    #[serde(default)]
    pub stream_control_method: StreamControlMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultPaths {
    pub disk_images: Option<PathBuf>,
    pub music_files: Option<PathBuf>,
    pub programs: Option<PathBuf>,
    /// Starting directory for the File Browser tab
    #[serde(default)]
    pub file_browser_start_dir: Option<PathBuf>,
    /// Starting directory for the Music Player tab
    #[serde(default)]
    pub music_player_start_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub auto_mount_and_run: bool,
    pub default_mount_mode: String,
    pub show_hidden_files: bool,
    #[serde(default = "default_song_duration")]
    pub default_song_duration: u32, // Default duration for songs without known length (in seconds)
    #[serde(default = "default_font_size")]
    pub font_size: u32, // Base font size for UI elements
}

fn default_font_size() -> u32 {
    12
}

fn default_song_duration() -> u32 {
    180 // 3 minutes default
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            connection: ConnectionSettings {
                host: String::new(), // Empty by default - user must configure
                password: None,
                stream_control_method: StreamControlMethod::default(),
            },
            default_paths: DefaultPaths {
                disk_images: None,
                music_files: None,
                programs: None,
                file_browser_start_dir: None,
                music_player_start_dir: None,
            },
            preferences: Preferences {
                auto_mount_and_run: false,
                default_mount_mode: String::from("readwrite"),
                show_hidden_files: false,
                default_song_duration: 180, // 3 minutes
                font_size: 12,
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
