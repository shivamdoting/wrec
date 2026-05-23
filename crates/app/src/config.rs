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
    #[serde(default)]
    pub(crate) show_nerd_logs: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            settings: RecorderSettings::default(),
            selected_target_key: None,
            show_nerd_logs: false,
        }
    }
}

impl AppConfig {
    pub(crate) fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|err| {
                tracing::warn!("failed to parse config: {err}");
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                load_legacy_config(&path).unwrap_or_default()
            }
            Err(err) => {
                tracing::warn!("failed to read config: {err}");
                Self::default()
            }
        }
    }
}

pub(crate) fn save_config(config: &AppConfig) -> std::io::Result<()> {
    write_config(&config_path(), config)
}

fn load_legacy_config(path: &Path) -> Option<AppConfig> {
    legacy_config_paths().into_iter().find_map(|legacy_path| {
        match fs::read_to_string(&legacy_path) {
            Ok(contents) => match serde_json::from_str::<AppConfig>(&contents) {
                Ok(config) => {
                    if let Err(err) = write_config(path, &config) {
                        tracing::warn!("failed to migrate config: {err}");
                    } else if let Err(err) = fs::remove_file(&legacy_path) {
                        tracing::warn!("failed to remove legacy config: {err}");
                    }
                    Some(config)
                }
                Err(err) => {
                    tracing::warn!("failed to parse legacy config: {err}");
                    None
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!("failed to read legacy config: {err}");
                None
            }
        }
    })
}

fn write_config(path: &Path, config: &AppConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(config)?;
    fs::write(path, json)
}

pub(crate) fn wrec_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".wrec"))
        .unwrap_or_else(|| Path::new(".").join(".wrec"))
}

fn config_path() -> PathBuf {
    wrec_dir().join("config.json")
}

fn legacy_config_paths() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            vec![
                home.join(".config").join("wrec").join("config.json"),
                home.join(".config").join("wrec.json"),
            ]
        })
        .unwrap_or_else(|| vec![Path::new(".").join("wrec.json")])
}
