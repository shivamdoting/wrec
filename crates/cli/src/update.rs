use std::{
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use serde_json::{json, Value};

use crate::args::UpdateArgs;

/// The same installer served at https://wrec.app/install, embedded so the
/// update path and the install path can never disagree.
const INSTALLER: &str = include_str!("../../../scripts/install-cli.sh");

const DEFAULT_REPO: &str = "shivamhwp/wrec";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn update(args: UpdateArgs) -> ExitCode {
    match run(&args) {
        Ok(code) => code,
        Err(message) => {
            if args.json {
                println!("{}", json!({ "command": "update", "error": message }));
            } else {
                eprintln!("error: {message}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(args: &UpdateArgs) -> Result<ExitCode, String> {
    let repo = std::env::var("WREC_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let asset = asset_name()?;
    let release = latest_release(&repo, &asset)?;
    let update_available = is_newer(&release.version, CURRENT_VERSION);

    if args.check {
        if args.json {
            println!(
                "{}",
                json!({
                    "command": "update",
                    "current": CURRENT_VERSION,
                    "latest": release.version,
                    "update_available": update_available,
                })
            );
        } else if update_available {
            println!(
                "update available: wrec {CURRENT_VERSION} -> {} (run `wrec update`)",
                release.version
            );
        } else {
            println!("wrec {CURRENT_VERSION} is up to date");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !update_available && !args.force {
        if args.json {
            println!(
                "{}",
                json!({
                    "command": "update",
                    "current": CURRENT_VERSION,
                    "latest": release.version,
                    "status": "up_to_date",
                })
            );
        } else {
            println!("wrec {CURRENT_VERSION} is up to date");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let work_dir = exclusive_work_dir()?;
    let result = download_and_install(args, &release, &asset, &work_dir);
    let _ = std::fs::remove_dir_all(&work_dir);
    let installed = result?;

    // A live socket means a daemon is (or was) running the old binary.
    let daemon_restart_required = control::socket_path().exists();
    if args.json {
        println!(
            "{}",
            json!({
                "command": "update",
                "current": CURRENT_VERSION,
                "latest": release.version,
                "status": "updated",
                "bin": installed,
                "daemon_restart_required": daemon_restart_required,
                "next": daemon_restart_required
                    .then_some("Run `wrec daemon stop` then `wrec daemon start` to pick up the new version."),
            })
        );
    } else {
        println!("updated wrec {CURRENT_VERSION} -> {}", release.version);
        if daemon_restart_required {
            println!(
                "the daemon may still be running the old version; restart it with `wrec daemon stop && wrec daemon start`"
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug)]
struct Release {
    version: String,
    download_url: String,
    sha256: Option<String>,
}

fn download_and_install(
    args: &UpdateArgs,
    release: &Release,
    asset: &str,
    work_dir: &Path,
) -> Result<String, String> {
    let archive = work_dir.join(asset);
    if !args.json {
        println!("downloading {}", release.download_url);
    }
    download(&release.download_url, &archive)?;

    let Some(expected) = &release.sha256 else {
        return Err(format!(
            "release asset {asset} publishes no SHA-256 digest; refusing to install an unverifiable archive"
        ));
    };
    let actual = sha256_of(&archive)?;
    if &actual != expected {
        return Err(format!(
            "checksum mismatch for {asset}: expected {expected}, got {actual}; not installing"
        ));
    }
    if !args.json {
        println!("verified sha256 {expected}");
    }

    let installer = work_dir.join("install-cli.sh");
    std::fs::write(&installer, INSTALLER)
        .map_err(|err| format!("could not write installer: {err}"))?;

    let status = Command::new("sh")
        .arg(&installer)
        .env("WREC_CLI_ARCHIVE", &archive)
        .stdin(Stdio::inherit())
        .stdout(if args.json {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stderr(Stdio::inherit())
        .status()
        .map_err(|err| format!("could not run installer: {err}"))?;
    if !status.success() {
        return Err(format!("installer exited with {status}"));
    }

    let prefix = std::env::var("WREC_PREFIX").unwrap_or_else(|_| "/usr/local".to_string());
    Ok(format!("{prefix}/bin/wrec"))
}

fn latest_release(repo: &str, asset: &str) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--retry",
            "3",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: wrec-cli",
            &url,
        ])
        .output()
        .map_err(|err| format!("could not run curl: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not query the latest release from {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let body: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("could not parse the release response: {err}"))?;
    parse_release(&body, asset)
}

fn parse_release(body: &Value, asset: &str) -> Result<Release, String> {
    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("release response had no tag_name")?;
    let version = tag.trim_start_matches('v').to_string();

    let entry = body
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(asset))
        })
        .ok_or_else(|| format!("release {tag} has no asset named {asset}"))?;

    let download_url = entry
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("asset {asset} has no download url"))?
        .to_string();
    let sha256 = entry
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_string);

    Ok(Release {
        version,
        download_url,
        sha256,
    })
}

fn asset_name() -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("wrec update only supports macOS".into());
    }
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    };
    let qualifier = std::env::var("WREC_ARTIFACT_QUALIFIER").unwrap_or_default();
    if qualifier.is_empty() {
        Ok(format!("wrec-cli-{target}.tar.gz"))
    } else {
        Ok(format!("wrec-cli-{target}-{qualifier}.tar.gz"))
    }
}

/// A fresh, owner-only workspace whose creation fails rather than adopting an
/// existing directory, so nothing can pre-place files we later trust.
fn exclusive_work_dir() -> Result<std::path::PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("wrec-update-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// Treats an unparseable version pair as an update when the strings differ, so
/// a rename of the tagging scheme never strands installed CLIs.
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
    // Ignore a pre-release suffix ("1-rc1" -> 1) so tags like 0.2.1-rc1
    // compare by their numeric patch instead of falling into the
    // strings-differ fallback, which could otherwise downgrade.
    let patch = parts.next()?;
    let digits: String = patch.chars().take_while(char::is_ascii_digit).collect();
    Some((major, minor, digits.parse().ok()?))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_detects_updates() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn unparseable_versions_update_when_different() {
        assert!(is_newer("2026.1", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
    }

    #[test]
    fn prerelease_suffixes_compare_by_numeric_patch() {
        assert!(!is_newer("0.2.1-rc1", "0.2.2"));
        assert!(is_newer("0.2.3-rc1", "0.2.2"));
        assert_eq!(parse_version("0.2.1-rc1"), Some((0, 2, 1)));
    }

    #[test]
    fn work_dir_creation_refuses_an_existing_directory() {
        let dir = exclusive_work_dir().unwrap();
        assert!(std::fs::create_dir(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_parsing_extracts_version_url_and_digest() {
        let body = serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "wrec-0.2.0.dmg",
                    "browser_download_url": "https://example.com/wrec-0.2.0.dmg",
                    "digest": "sha256:aaaa"
                },
                {
                    "name": "wrec-cli-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/cli.tar.gz",
                    "digest": "sha256:bbbb"
                }
            ]
        });

        let release = parse_release(&body, "wrec-cli-aarch64-apple-darwin.tar.gz").unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.download_url, "https://example.com/cli.tar.gz");
        assert_eq!(release.sha256.as_deref(), Some("bbbb"));
    }

    #[test]
    fn release_parsing_reports_missing_assets() {
        let body = serde_json::json!({ "tag_name": "v0.2.0", "assets": [] });

        let err = parse_release(&body, "wrec-cli-aarch64-apple-darwin.tar.gz").unwrap_err();
        assert!(err.contains("no asset named"));
    }

    #[test]
    fn release_parsing_tolerates_missing_digest() {
        let body = serde_json::json!({
            "tag_name": "v0.3.0",
            "assets": [{
                "name": "wrec-cli-aarch64-apple-darwin.tar.gz",
                "browser_download_url": "https://example.com/cli.tar.gz"
            }]
        });

        let release = parse_release(&body, "wrec-cli-aarch64-apple-darwin.tar.gz").unwrap();
        assert_eq!(release.sha256, None);
    }
}
