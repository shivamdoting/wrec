use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const CLI_INSTALLER_URL: &str = "https://wrec-beta.vercel.app/install";
const MANAGED_CLI_MARKER: &str = "# managed by wrec";
const INSTALLED_BIN: &str = "/usr/local/bin/wrec";
const INSTALLED_CLI: &str = "/usr/local/lib/wrec/wrec";
const INSTALLED_DAEMON: &str = "/usr/local/lib/wrec/daemon";
const INSTALLED_CAPTURE_ENGINE: &str = "/usr/local/lib/wrec/capture-engine";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliInstallStatus {
    NotInstalled,
    Installed,
    NeedsUpdate,
    Conflict,
}

impl CliInstallStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NotInstalled => "Not installed",
            Self::Installed => "Installed",
            Self::NeedsUpdate => "Update available",
            Self::Conflict => "Path conflict",
        }
    }
}

pub(crate) fn choose_output_dir() -> Option<PathBuf> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"POSIX path of (choose folder with prompt "Choose recording folder")"#,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn open_path(path: &Path) -> std::io::Result<()> {
    Command::new("open").arg(path).spawn().map(|_| ())
}

pub(crate) fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("open").arg(url).spawn().map(|_| ())
}

pub(crate) fn cli_install_status() -> CliInstallStatus {
    let installed_bin = Path::new(INSTALLED_BIN);
    if !installed_bin.exists() {
        return CliInstallStatus::NotInstalled;
    }
    if !managed_cli_bin(installed_bin) {
        return CliInstallStatus::Conflict;
    }

    if [INSTALLED_CLI, INSTALLED_DAEMON, INSTALLED_CAPTURE_ENGINE]
        .into_iter()
        .all(|path| Path::new(path).is_file())
    {
        CliInstallStatus::Installed
    } else {
        CliInstallStatus::NeedsUpdate
    }
}

pub(crate) fn cli_install_command() -> Option<String> {
    let version = current_app_bundle_path()
        .as_deref()
        .filter(|app| !is_dev_app(app))
        .map(|_| env!("CARGO_PKG_VERSION"));

    Some(match version {
        Some(version) => format!("curl -fsSL {CLI_INSTALLER_URL} | WREC_VERSION={version} sh"),
        None => format!("curl -fsSL {CLI_INSTALLER_URL} | sh"),
    })
}

fn current_app_bundle_path() -> Option<PathBuf> {
    std::env::current_exe().ok()?.ancestors().find_map(|path| {
        let name = path.file_name()?.to_str()?;
        name.ends_with(".app").then(|| path.to_path_buf())
    })
}

fn is_dev_app(app: &Path) -> bool {
    let name_is_dev = app
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("Dev"));
    let info_is_dev = fs::read_to_string(app.join("Contents").join("Info.plist"))
        .is_ok_and(|info| info.contains("app.wrec.dev") || info.contains("Wrec Dev"));

    name_is_dev || info_is_dev
}

fn managed_cli_bin(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|contents| contents.contains(MANAGED_CLI_MARKER))
}
