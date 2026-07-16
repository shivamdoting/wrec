//! In-place app self-update.
//!
//! The app downloads the release archive itself (curl applies no quarantine
//! xattr), verifies the release asset's SHA-256 digest, stages the new bundle
//! next to the current one, and swaps them with two same-directory renames so
//! the operation is atomic per rename and can be rolled back. A detached
//! helper relaunches the new bundle after this process exits.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::Value;

const DEFAULT_REPO: &str = "shivamhwp/wrec";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppUpdateState {
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Updating {
        version: String,
    },
    /// A failed check; a failed install reverts to `Available` so the
    /// button still offers the install.
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ReadyUpdate {
    pub(crate) version: String,
    pub(crate) bundle: PathBuf,
    pub(crate) old_bundle: PathBuf,
}

/// Preview hooks under the wrec data dir (for dev builds:
/// `~/Library/Application Support/Wrec Dev/`):
///
/// - `mock-latest-version` — a version number the About tab treats as the
///   latest release. Higher than the build shows the update button; equal or
///   lower shows up to date. On its own, installing is refused (UI preview).
/// - `mock-latest-archive` — a path to a local `.tar.gz` containing a
///   packaged `.app`. With both files set and the app running from a
///   packaged bundle, clicking Update runs the REAL pipeline against that
///   archive: extract, validate, daemon stop, bundle swap, relaunch.
///
/// `scripts/preview-app-update.sh` sets all of this up with the dev build.
/// Delete the files to restore real release-driven behavior.
fn mock_latest() -> Option<String> {
    let contents = std::fs::read_to_string(mock_file("mock-latest-version")?).ok()?;
    parse_mock(&contents)
}

fn mock_archive() -> Option<PathBuf> {
    let contents = std::fs::read_to_string(mock_file("mock-latest-archive")?).ok()?;
    parse_mock(&contents).map(PathBuf::from)
}

/// Dev (debug-profile) builds only, matching the data-dir namespace split:
/// release builds must never let a user-writable file redirect or block
/// real updates.
fn mock_file(name: &str) -> Option<PathBuf> {
    cfg!(debug_assertions).then(|| config::wrec_dir().join(name))
}

fn parse_mock(contents: &str) -> Option<String> {
    let version = contents.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// The `.app` bundle this process runs from, if any.
fn current_bundle() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("app"))
            .map(Path::to_path_buf)
    })
}

/// The bundle this process runs from, when it is one the updater may replace:
/// a packaged, non-dev `.app`. Dev bundles and bare Cargo binaries update
/// through a rebuild instead.
pub(crate) fn eligible_bundle() -> Result<PathBuf, String> {
    // With the mock set, the update row stays active even in dev builds so
    // the update flow can be previewed.
    if mock_latest().is_some() {
        return Ok(current_bundle().unwrap_or_default());
    }
    let bundle =
        current_bundle().ok_or("not running from an app bundle; update by rebuilding instead")?;

    let is_dev = bundle
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("Dev"));
    if is_dev {
        return Err("dev builds update by rebuilding, not in place".into());
    }
    Ok(bundle)
}

/// Returns `Some(version)` when a newer release exists, `None` when current.
pub(crate) fn check() -> Result<Option<String>, String> {
    if let Some(mock) = mock_latest() {
        return Ok(is_newer(&mock, CURRENT_VERSION).then_some(mock));
    }
    eligible_bundle()?;
    let release = latest_release()?;
    Ok(is_newer(&release.version, CURRENT_VERSION).then_some(release.version))
}

pub(crate) fn download_and_apply() -> Result<ReadyUpdate, String> {
    if let Some(version) = mock_latest() {
        let Some(archive) = mock_archive() else {
            return Err("mock-latest-version is set without mock-latest-archive (UI preview); \
                run scripts/preview-app-update.sh for the full flow, or delete the file for real updates"
                .into());
        };
        if !archive.is_file() {
            return Err(format!(
                "mock-latest-archive points at {}, which does not exist",
                archive.display()
            ));
        }
        let bundle = current_bundle().ok_or(
            "the mock update needs the app to run from a packaged bundle; \
                open dist/dev/Wrec Dev.app instead of cargo run",
        )?;
        let work_dir = exclusive_work_dir()?;
        let result = apply_archive(&bundle, &archive, &work_dir, &version);
        let _ = std::fs::remove_dir_all(&work_dir);
        return result;
    }

    let bundle = eligible_bundle()?;
    let release = latest_release()?;
    if !is_newer(&release.version, CURRENT_VERSION) {
        return Err(format!("wrec {CURRENT_VERSION} is already up to date"));
    }
    let asset = release
        .asset
        .as_ref()
        .ok_or_else(|| format!("release v{} has no app archive asset yet", release.version))?;

    let work_dir = exclusive_work_dir()?;
    let result = fetch_and_apply(&bundle, asset, &work_dir, &release.version);
    let _ = std::fs::remove_dir_all(&work_dir);
    result
}

/// Relaunches the swapped-in bundle once this process exits and removes the
/// old bundle afterwards. Must be called right before quitting.
pub(crate) fn relaunch_and_cleanup(update: &ReadyUpdate) {
    // Paths are passed as positional parameters, never interpolated into the
    // script, so a hostile install path cannot inject shell commands.
    let _ = Command::new("sh")
        .args(["-c", r#"sleep 1; open -n "$0"; sleep 5; rm -rf "$1""#])
        .arg(&update.bundle)
        .arg(&update.old_bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn fetch_and_apply(
    bundle: &Path,
    asset: &ReleaseAsset,
    work_dir: &Path,
    version: &str,
) -> Result<ReadyUpdate, String> {
    let archive = work_dir.join("wrec-app.tar.gz");
    download(&asset.url, &archive)?;

    let Some(expected) = &asset.sha256 else {
        return Err(
            "the release asset publishes no SHA-256 digest; refusing to install an unverifiable archive"
                .into(),
        );
    };
    let actual = sha256_of(&archive)?;
    if &actual != expected {
        return Err(format!(
            "checksum mismatch for the app archive: expected {expected}, got {actual}; not installing"
        ));
    }

    apply_archive(bundle, &archive, work_dir, version)
}

/// The pipeline from a verified archive onward — shared by real updates and
/// the mock preview, so the preview exercises exactly what ships.
fn apply_archive(
    bundle: &Path,
    archive: &Path,
    work_dir: &Path,
    version: &str,
) -> Result<ReadyUpdate, String> {
    let extracted_dir = work_dir.join("extracted");
    std::fs::create_dir_all(&extracted_dir)
        .map_err(|err| format!("could not create {}: {err}", extracted_dir.display()))?;
    run_ok(
        Command::new("/usr/bin/tar")
            .args(["-xzf"])
            .arg(&archive)
            .arg("-C")
            .arg(&extracted_dir),
        "could not extract the app archive",
    )?;
    let new_bundle = find_app_bundle(&extracted_dir)?;

    // The daemon ships inside the bundle; stop it so the relaunched app
    // starts a matching one. A refusal (active or queued recording) arrives
    // as a successful response carrying an error, not as Err — Err only
    // means the daemon was unreachable, i.e. nothing to stop.
    if let Ok(response) = control::send_request("daemon.stop", serde_json::json!({})) {
        if let Some(err) = response.error {
            if err.code == "daemon_busy" {
                return Err(format!("{} {}", err.message, err.next));
            }
        }
    }

    let old_bundle = swap_bundles(bundle, &new_bundle)?;
    Ok(ReadyUpdate {
        version: version.to_string(),
        bundle: bundle.to_path_buf(),
        old_bundle,
    })
}

/// Replaces `bundle` with `replacement` using a staged copy in the same
/// directory (same-volume renames are atomic). Returns where the old bundle
/// was moved; restores it if the final rename fails.
fn swap_bundles(bundle: &Path, replacement: &Path) -> Result<PathBuf, String> {
    let parent = bundle
        .parent()
        .ok_or("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("app bundle has no name")?;
    let pid = std::process::id();
    let staged = parent.join(format!(".{name}.staged-{pid}"));
    let old = parent.join(format!(".{name}.old-{pid}"));

    // ditto preserves permissions, symlinks, and signatures across volumes.
    run_ok(
        Command::new("/usr/bin/ditto").arg(replacement).arg(&staged),
        "could not stage the new app bundle (is the install directory writable?)",
    )?;

    if let Err(err) = std::fs::rename(bundle, &old) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(format!("could not move the current app aside: {err}"));
    }
    if let Err(err) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&old, bundle);
        let _ = std::fs::remove_dir_all(&staged);
        return Err(format!("could not move the new app into place: {err}"));
    }
    Ok(old)
}

fn find_app_bundle(dir: &Path) -> Result<PathBuf, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|err| format!("could not read {}: {err}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("app") {
            let executable = path.join("Contents").join("MacOS").join("wrec-app");
            if executable.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "archive bundle {} is missing Contents/MacOS/wrec-app",
                path.display()
            ));
        }
    }
    Err("the app archive contained no .app bundle".into())
}

#[derive(Debug)]
struct Release {
    version: String,
    asset: Option<ReleaseAsset>,
}

#[derive(Debug)]
struct ReleaseAsset {
    url: String,
    sha256: Option<String>,
}

fn latest_release() -> Result<Release, String> {
    let repo = std::env::var("WREC_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "3",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: wrec-app",
            &url,
        ])
        .output()
        .map_err(|err| format!("could not run curl: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not query the latest release: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let body: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("could not parse the release response: {err}"))?;
    parse_release(&body)
}

fn parse_release(body: &Value) -> Result<Release, String> {
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("release response had no tag_name")?;
    let version = tag.trim_start_matches('v').to_string();

    let asset_name = app_asset_name();
    let asset = body
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets.iter().find(|entry| {
                entry.get("name").and_then(Value::as_str) == Some(asset_name.as_str())
            })
        })
        .and_then(|entry| {
            Some(ReleaseAsset {
                url: entry.get("browser_download_url")?.as_str()?.to_string(),
                sha256: entry
                    .get("digest")
                    .and_then(Value::as_str)
                    .and_then(|digest| digest.strip_prefix("sha256:"))
                    .map(str::to_string),
            })
        });

    Ok(Release { version, asset })
}

fn app_asset_name() -> String {
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    };
    format!("wrec-app-{target}.tar.gz")
}

/// Treats an unparseable version pair as an update when the strings differ, so
/// a rename of the tagging scheme never strands installed apps.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => latest != current,
    }
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim_start_matches('v').splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // Ignore a pre-release suffix ("1-rc1" -> 1) so such tags compare by
    // their numeric patch instead of the strings-differ fallback.
    let patch = parts.next()?;
    let digits: String = patch.chars().take_while(char::is_ascii_digit).collect();
    Some((major, minor, digits.parse().ok()?))
}

/// A fresh, owner-only workspace whose creation fails rather than adopting an
/// existing directory, so nothing can pre-place files we later trust.
fn exclusive_work_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("wrec-app-update-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|err| format!("could not run shasum: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "shasum failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "shasum produced no output".into())
}

fn download(url: &str, output: &Path) -> Result<(), String> {
    let result = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(output)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("could not run curl: {err}"))?;
    if !result.status.success() {
        return Err(format!(
            "download failed for {url}: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(())
}

fn run_ok(command: &mut Command, context: &str) -> Result<(), String> {
    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("{context}: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wrec-updater-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_marker(bundle: &Path, contents: &str) {
        std::fs::create_dir_all(bundle).unwrap();
        std::fs::write(bundle.join("marker"), contents).unwrap();
    }

    #[test]
    fn mock_contents_are_trimmed_and_empty_is_ignored() {
        assert_eq!(parse_mock("0.3.0\n"), Some("0.3.0".to_string()));
        assert_eq!(parse_mock("  v0.3.0  "), Some("v0.3.0".to_string()));
        assert_eq!(parse_mock("\n  \n"), None);
    }

    #[test]
    fn version_ordering_detects_updates() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("2026.1", "0.2.0"));
    }

    #[test]
    fn swap_replaces_bundle_and_keeps_the_old_one() {
        let dir = temp_dir("swap");
        let bundle = dir.join("Wrec.app");
        let replacement = dir.join("New.app");
        write_marker(&bundle, "old");
        write_marker(&replacement, "new");

        let old = swap_bundles(&bundle, &replacement).unwrap();

        assert_eq!(
            std::fs::read_to_string(bundle.join("marker")).unwrap(),
            "new"
        );
        assert_eq!(std::fs::read_to_string(old.join("marker")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swap_fails_cleanly_when_replacement_is_missing() {
        let dir = temp_dir("swap-missing");
        let bundle = dir.join("Wrec.app");
        write_marker(&bundle, "old");

        let err = swap_bundles(&bundle, &dir.join("Nope.app")).unwrap_err();

        assert!(err.contains("could not stage"));
        assert_eq!(
            std::fs::read_to_string(bundle.join("marker")).unwrap(),
            "old"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_parsing_reads_tag_and_optional_asset() {
        let asset_name = app_asset_name();
        let body = serde_json::json!({
            "tag_name": "v0.3.0",
            "assets": [{
                "name": asset_name,
                "browser_download_url": "https://example.com/app.tar.gz",
                "digest": "sha256:abcd"
            }]
        });
        let release = parse_release(&body).unwrap();
        assert_eq!(release.version, "0.3.0");
        let asset = release.asset.unwrap();
        assert_eq!(asset.url, "https://example.com/app.tar.gz");
        assert_eq!(asset.sha256.as_deref(), Some("abcd"));

        let no_asset = serde_json::json!({ "tag_name": "v0.3.0", "assets": [] });
        assert!(parse_release(&no_asset).unwrap().asset.is_none());
    }

    #[test]
    fn app_bundle_discovery_requires_the_executable() {
        let dir = temp_dir("find");
        let bundle = dir.join("Wrec.app");
        std::fs::create_dir_all(bundle.join("Contents").join("MacOS")).unwrap();

        assert!(find_app_bundle(&dir).is_err());

        std::fs::write(
            bundle.join("Contents").join("MacOS").join("wrec-app"),
            "stub",
        )
        .unwrap();
        assert_eq!(find_app_bundle(&dir).unwrap(), bundle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
