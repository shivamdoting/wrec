use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use wrec_core::RecorderSettings;

// Keep app, CLI, and dev/release builds on one namespace so selected settings stay shared.
const APP_DATA_DIR_NAME: &str = "Wrec";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub settings: RecorderSettings,
    pub selected_target_key: Option<String>,
    #[serde(default)]
    pub show_nerd_logs: bool,
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
    pub fn load() -> Self {
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

pub fn save_config(config: &AppConfig) -> std::io::Result<()> {
    write_config(&config_path(), config)
}

pub fn store_path() -> PathBuf {
    wrec_dir().join("wrec.sqlite")
}

pub fn log_path() -> PathBuf {
    wrec_dir().join("wrec.log")
}

pub fn wrec_dir() -> PathBuf {
    std::env::var_os("WREC_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_wrec_dir)
}

pub fn config_path() -> PathBuf {
    wrec_dir().join("config.json")
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

#[cfg(target_os = "macos")]
fn default_wrec_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            home.join("Library")
                .join("Application Support")
                .join(APP_DATA_DIR_NAME)
        })
        .unwrap_or_else(|| Path::new(".").join(APP_DATA_DIR_NAME))
}

#[cfg(not(target_os = "macos"))]
fn default_wrec_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".wrec"))
        .unwrap_or_else(|| Path::new(".").join(".wrec"))
}

fn legacy_config_paths() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            let mut paths = legacy_app_support_config_paths(&home);
            paths.extend([
                home.join(".wrec").join("config.json"),
                home.join(".config").join("wrec").join("config.json"),
                home.join(".config").join("wrec.json"),
            ]);
            paths
        })
        .unwrap_or_else(|| vec![Path::new(".").join("wrec.json")])
}

#[cfg(target_os = "macos")]
fn legacy_app_support_config_paths(home: &Path) -> Vec<PathBuf> {
    let app_support = home.join("Library").join("Application Support");
    let mut names = vec!["Wrec Dev".to_string()];
    let runtime_name = runtime_app_name();
    if runtime_name != APP_DATA_DIR_NAME && runtime_name != "Wrec Dev" {
        names.push(runtime_name);
    }

    names
        .into_iter()
        .map(|name| app_support.join(name).join("config.json"))
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn legacy_app_support_config_paths(_: &Path) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn runtime_app_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.ancestors()
                .filter_map(|path| path.file_name()?.to_str())
                .find_map(|name| name.strip_suffix(".app").map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| "Wrec".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wrec_core::Resolution;

    #[test]
    fn missing_resolution_uses_recorder_settings_default() {
        let config = serde_json::from_str::<AppConfig>(
            r#"{
                "settings": {
                    "source": "Display",
                    "fps": "Fps30",
                    "codec": "Hevc",
                    "quality": "High",
                    "output_dir": "/tmp/wrec",
                    "include_cursor": true,
                    "include_system_audio": true,
                    "hide_wrec": true
                },
                "selected_target_key": null
            }"#,
        )
        .expect("old config without resolution should still load");

        assert_eq!(config.settings.resolution, Resolution::R1080p);
    }
}
