use domain::RecorderSettings;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

// Dev (debug-profile) builds get their own data namespace so testing never
// touches the installed app's store, config, or logs. App, CLI, and daemon
// built with the same profile stay on one namespace.
#[cfg(debug_assertions)]
const APP_DATA_DIR_NAME: &str = "Wrec Dev";
#[cfg(not(debug_assertions))]
const APP_DATA_DIR_NAME: &str = "Wrec";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub settings: RecorderSettings,
    pub selected_target_key: Option<String>,
    #[serde(default)]
    pub show_nerd_logs: bool,
    #[serde(default)]
    pub auto_open_after_recording: bool,
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

const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

/// File-backed log writer that renames the file to `<name>.old` (one
/// generation) once it exceeds the size cap, so a pathological error loop
/// cannot grow a log without bound.
struct RotatingLogWriter {
    file: fs::File,
    path: PathBuf,
    written: u64,
}

impl RotatingLogWriter {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            written,
        })
    }

    fn rotate_if_needed(&mut self) {
        if self.written < LOG_ROTATE_BYTES {
            return;
        }
        let mut old = self.path.clone().into_os_string();
        old.push(".old");
        // Best effort: if a previous rotation already renamed the file but
        // failed to reopen it, the source is gone and this rename fails while
        // the reopen below still recovers the primary path.
        let _ = fs::rename(&self.path, PathBuf::from(old));
        match Self::open(&self.path) {
            // A fresh handle only helps if the rename actually made room;
            // otherwise this reopened the same over-cap file.
            Ok(fresh) if fresh.written < LOG_ROTATE_BYTES => *self = fresh,
            // Keep the current handle and wait another cap's worth before
            // retrying, so a persistent failure adds no per-write churn.
            _ => self.written = 0,
        }
    }
}

impl std::io::Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.rotate_if_needed();
        let written = self.file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Initialize the process-wide tracing subscriber writing to `path`, with
/// `RUST_LOG`-style filtering (default `info`) and size-capped rotation.
/// Falls back to stderr if the file cannot be opened.
pub fn init_file_tracing(path: &Path) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match RotatingLogWriter::open(path) {
        Ok(writer) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(writer))
                .try_init();
        }
        Err(err) => {
            eprintln!("failed to open log file {}: {err}", path.display());
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .try_init();
        }
    }
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
    // "Wrec Dev" is the live dev-build namespace, not a legacy source; a
    // migration would delete the dev config out from under dev builds.
    let runtime_name = runtime_app_name();
    let mut names = Vec::new();
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
    use domain::Resolution;

    #[test]
    fn rotating_writer_moves_full_log_aside_and_starts_fresh() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("wrec-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.log");
        let old_path = dir.join("test.log.old");
        std::fs::write(&path, vec![b'x'; LOG_ROTATE_BYTES as usize]).unwrap();

        let mut writer = RotatingLogWriter::open(&path).unwrap();
        writer.write_all(b"after rotation\n").unwrap();

        assert!(old_path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after rotation\n");

        let _ = std::fs::remove_dir_all(dir);
    }

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
        assert!(!config.settings.include_microphone);
        assert!(!config.auto_open_after_recording);
    }
}
