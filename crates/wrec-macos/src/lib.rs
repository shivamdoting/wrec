use wrec_core::{
    CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderError, RecorderSettings,
    RecordingSession, Result,
};

#[derive(Default)]
pub struct MacosRecorder {
    active: Option<RecordingSession>,
}

impl MacosRecorder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RecorderEngine for MacosRecorder {
    fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        platform::list_targets()
    }

    fn start(
        &mut self,
        target: CaptureTarget,
        settings: RecorderSettings,
    ) -> Result<RecordingSession> {
        let session = platform::start_recording(target, settings)?;
        self.active = Some(session.clone());
        Ok(session)
    }

    fn stop(&mut self) -> Result<()> {
        platform::stop_recording()?;
        self.active = None;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static CHILD: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        use std::process::Command;

        let output = Command::new("xcrun")
            .arg("swift")
            .arg(helper_path())
            .arg("--list")
            .output()
            .map_err(|err| RecorderError::Backend(format!("failed to list targets: {err}")))?;

        if !output.status.success() {
            return Err(RecorderError::Backend(format!(
                "target listing failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let mut targets = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut parts = line.splitn(3, '\t');
            let kind = match parts.next() {
                Some("display") => CaptureSourceKind::Display,
                Some("window") => CaptureSourceKind::Window,
                _ => continue,
            };
            let Some(id) = parts.next().and_then(|id| id.parse::<u64>().ok()) else {
                continue;
            };
            let name = parts.next().unwrap_or("Unknown").to_string();
            targets.push(CaptureTarget { id, name, kind });
        }

        if targets.is_empty() {
            targets.push(CaptureTarget {
                id: 0,
                name: "Main Display".to_string(),
                kind: CaptureSourceKind::Display,
            });
        }
        Ok(targets)
    }

    pub fn start_recording(
        target: CaptureTarget,
        settings: RecorderSettings,
    ) -> Result<RecordingSession> {
        use std::process::{Command, Stdio};

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        if child_slot.lock().unwrap().is_some() {
            return Err(RecorderError::Backend("recording is already active".into()));
        }

        std::fs::create_dir_all(&settings.output_dir)
            .map_err(|err| RecorderError::Backend(err.to_string()))?;

        let filename = format!("wrec-{}.mov", chrono_like_timestamp());
        let output_path = settings.output_dir.join(filename);
        let helper = helper_path();

        // Temporary v0 native bridge: run a tiny Swift helper that uses
        // ScreenCaptureKit + SCRecordingOutput. The frame path stays inside
        // Apple's native stack; Rust never receives/copies pixels.
        let child = Command::new("xcrun")
            .arg("swift")
            .arg(helper)
            .arg(&output_path)
            .arg(settings.fps.as_u32().to_string())
            .arg(if settings.include_cursor {
                "true"
            } else {
                "false"
            })
            .arg(match target.kind {
                CaptureSourceKind::Display => "display",
                CaptureSourceKind::Window => "window",
            })
            .arg(target.id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| RecorderError::Backend(format!("failed to start helper: {err}")))?;

        tracing::info!(?target, ?settings, ?output_path, "started recording helper");
        *child_slot.lock().unwrap() = Some(child);
        Ok(RecordingSession { output_path })
    }

    pub fn stop_recording() -> Result<()> {
        use std::io::Write;

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let Some(mut child) = child_slot.lock().unwrap().take() else {
            return Ok(());
        };

        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(b"stop\n");
        }

        let status = child
            .wait()
            .map_err(|err| RecorderError::Backend(format!("failed waiting for helper: {err}")))?;
        if !status.success() {
            return Err(RecorderError::Backend(format!(
                "recording helper exited with {status}"
            )));
        }
        Ok(())
    }

    fn helper_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("native")
            .join("wrec_helper.swift")
    }

    fn chrono_like_timestamp() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        secs.to_string()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn start_recording(
        _target: CaptureTarget,
        _settings: RecorderSettings,
    ) -> Result<RecordingSession> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn stop_recording() -> Result<()> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }
}
