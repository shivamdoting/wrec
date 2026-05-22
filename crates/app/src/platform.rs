use std::{
    path::{Path, PathBuf},
    process::Command,
};

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
