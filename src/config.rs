use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
/// Persisted UI/application settings for Photograph.
pub struct AppConfig {
    pub window_width: Option<f32>,
    pub window_height: Option<f32>,
    pub browse_path: Option<PathBuf>,
    pub preview_backend: Option<String>,
    pub tools_window_x: Option<f32>,
    pub tools_window_y: Option<f32>,
}

impl AppConfig {
    /// Returns the user config file path, if a config directory is available.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("photograph").join("config.toml"))
    }

    /// Loads config from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&contents).unwrap_or_default()
    }

    /// Writes config to disk, ignoring filesystem/serialization errors.
    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(&path, s);
        }
    }
}
