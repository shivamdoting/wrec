use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use wrec_core::RecorderSettings;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) settings: RecorderSettings,
    pub(crate) selected_target_key: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            settings: RecorderSettings::default(),
            selected_target_key: None,
        }
    }
}

impl AppConfig {
    pub(crate) fn load() -> Self {
        match fs::read_to_string(config_path()) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|err| {
                tracing::warn!("failed to parse config: {err}");
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!("failed to read config: {err}");
                Self::default()
            }
        }
    }
}

pub(crate) fn save_config(config: &AppConfig) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(config)?;
    fs::write(path, json)
}

fn config_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config").join("wrec.json"))
        .unwrap_or_else(|| Path::new(".").join("wrec.json"))
}
