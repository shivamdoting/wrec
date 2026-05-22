use wrec_core::{
    CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderError, RecorderMetrics,
    RecorderSettings, RecordingSession, Result,
};

static NEXT_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    Log {
        session_id: Option<u64>,
        message: String,
    },
    Metrics {
        session_id: u64,
        metrics: RecorderMetrics,
    },
    Failed {
        session_id: Option<u64>,
        message: String,
    },
    Exited {
        session_id: u64,
        success: bool,
        status: String,
    },
}

#[derive(Default)]
pub struct MacosRecorder {
    active: Option<RecordingSession>,
    events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
}

impl MacosRecorder {
    pub fn new(events: std::sync::mpsc::Sender<RecorderEvent>) -> Self {
        Self {
            active: None,
            events: Some(events),
        }
    }

    fn emit(&self, event: RecorderEvent) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }

    fn emit_log(&self, session_id: Option<u64>, message: impl Into<String>) {
        self.emit(RecorderEvent::Log {
            session_id,
            message: message.into(),
        });
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
        let session_id = NEXT_SESSION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.emit_log(
            Some(session_id),
            format!("starting capture: {} ({:?})", target.name, target.kind),
        );
        let session = platform::start_recording(session_id, target, settings, self.events.clone())?;
        self.active = Some(session.clone());
        self.emit_log(
            Some(session.id),
            format!("recording output: {}", session.output_path.display()),
        );
        Ok(session)
    }

    fn stop(&mut self) -> Result<()> {
        let session_id = self.active.as_ref().map(|session| session.id);
        self.emit_log(session_id, "stopping recording");
        platform::stop_recording()?;
        self.active = None;
        self.emit_log(session_id, "recording stopped");
        Ok(())
    }
}

impl Drop for MacosRecorder {
    fn drop(&mut self) {
        let _ = platform::stop_recording();
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant};

    const STOP_TIMEOUT: Duration = Duration::from_secs(20);
    const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

    struct RecordingProcess {
        child: std::process::Child,
        metrics_running: Arc<AtomicBool>,
    }

    static CHILD: OnceLock<Mutex<Option<RecordingProcess>>> = OnceLock::new();

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        use std::process::Command;

        let output = Command::new(helper_path())
            .arg("--list")
            .output()
            .map_err(|err| RecorderError::Backend(format!("failed to list targets: {err}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if is_permission_error(&stderr) {
                return Err(RecorderError::MissingScreenRecordingPermission);
            }
            return Err(RecorderError::Backend(format!(
                "target listing failed: {stderr}"
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
        session_id: u64,
        target: CaptureTarget,
        settings: RecorderSettings,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) -> Result<RecordingSession> {
        use std::process::{Command, Stdio};

        std::fs::create_dir_all(&settings.output_dir)
            .map_err(|err| RecorderError::Backend(err.to_string()))?;

        let filename = format!("wrec-{}.mov", chrono_like_timestamp());
        let output_path = settings.output_dir.join(filename);
        let helper = helper_path();
        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let mut active_child = child_slot.lock().unwrap();
        if active_child.is_some() {
            return Err(RecorderError::Backend("recording is already active".into()));
        }

        // Temporary v0 native bridge: run a compiled Swift helper that uses
        // ScreenCaptureKit + AVAssetWriter. The frame path stays inside
        // Apple's native stack; Rust never receives/copies pixels.
        let mut child = Command::new(helper)
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
            .arg(settings.codec.as_arg())
            .arg(settings.quality.as_arg())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| RecorderError::Backend(format!("failed to start helper: {err}")))?;

        let metrics_running = Arc::new(AtomicBool::new(true));
        let metrics_events = events.clone();
        let stderr = child.stderr.take();

        *active_child = Some(RecordingProcess {
            child,
            metrics_running: metrics_running.clone(),
        });
        drop(active_child);

        spawn_metrics_thread(
            session_id,
            output_path.clone(),
            metrics_running.clone(),
            metrics_events,
        );
        if let Some(stderr) = stderr {
            std::thread::spawn(move || forward_helper_stderr(session_id, stderr, events));
        }

        tracing::info!(?target, ?settings, ?output_path, "started recording helper");
        Ok(RecordingSession {
            id: session_id,
            output_path,
        })
    }

    pub fn stop_recording() -> Result<()> {
        use std::io::Write;

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let Some(mut process) = child_slot.lock().unwrap().take() else {
            return Ok(());
        };

        if let Some(stdin) = process.child.stdin.as_mut() {
            let _ = stdin.write_all(b"stop\n");
        }

        let started_waiting = Instant::now();
        let status =
            loop {
                if let Some(status) = process.child.try_wait().map_err(|err| {
                    RecorderError::Backend(format!("failed polling helper: {err}"))
                })? {
                    break status;
                }

                if started_waiting.elapsed() >= STOP_TIMEOUT {
                    let _ = process.child.kill();
                    let status = process.child.wait().map_err(|err| {
                        RecorderError::Backend(format!("failed killing stuck helper: {err}"))
                    })?;
                    process.metrics_running.store(false, Ordering::Relaxed);
                    return Err(RecorderError::Backend(format!(
                        "recording helper did not stop within {}s and was killed with {status}",
                        STOP_TIMEOUT.as_secs()
                    )));
                }

                std::thread::sleep(STOP_POLL_INTERVAL);
            };
        process.metrics_running.store(false, Ordering::Relaxed);
        if !status.success() {
            return Err(RecorderError::Backend(format!(
                "recording helper exited with {status}"
            )));
        }
        Ok(())
    }

    fn forward_helper_stderr(
        session_id: u64,
        stderr: std::process::ChildStderr,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            eprintln!("{line}");
            emit(
                &events,
                if helper_line_is_failure(&line) {
                    RecorderEvent::Failed {
                        session_id: Some(session_id),
                        message: line,
                    }
                } else {
                    RecorderEvent::Log {
                        session_id: Some(session_id),
                        message: line,
                    }
                },
            );
        }

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let Ok(mut child) = child_slot.lock() else {
            return;
        };
        let Some(status) = child
            .as_mut()
            .and_then(|process| {
                process.child.try_wait().ok().map(|status| {
                    if status.is_some() {
                        process.metrics_running.store(false, Ordering::Relaxed);
                    }
                    status
                })
            })
            .flatten()
        else {
            return;
        };
        *child = None;
        emit(
            &events,
            RecorderEvent::Exited {
                session_id,
                success: status.success(),
                status: status.to_string(),
            },
        );
    }

    fn emit(events: &Option<std::sync::mpsc::Sender<RecorderEvent>>, event: RecorderEvent) {
        if let Some(events) = events {
            let _ = events.send(event);
        }
    }

    fn spawn_metrics_thread(
        session_id: u64,
        output_path: std::path::PathBuf,
        running: Arc<AtomicBool>,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) {
        std::thread::spawn(move || {
            let started_at = Instant::now();
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let elapsed_secs = started_at.elapsed().as_secs();
                if elapsed_secs == 0 {
                    continue;
                }

                let output_bytes = std::fs::metadata(&output_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default();
                let estimated_bitrate_mbps =
                    output_bytes as f32 * 8. / elapsed_secs as f32 / 1_000_000.;

                emit(
                    &events,
                    RecorderEvent::Metrics {
                        session_id,
                        metrics: RecorderMetrics {
                            elapsed_secs,
                            output_bytes,
                            estimated_bitrate_mbps,
                        },
                    },
                );
            }
        });
    }

    fn helper_line_is_failure(line: &str) -> bool {
        line.contains("recording failed")
            || line.contains("wrec-helper: error:")
            || line.contains("permission")
            || line.contains("timed out")
            || line.contains("not found")
            || line.contains("no display")
    }

    fn is_permission_error(message: &str) -> bool {
        message.contains("permission denied") || message.contains("Screen Recording access")
    }

    fn helper_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("WREC_HELPER_PATH"))
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
        _session_id: u64,
        _target: CaptureTarget,
        _settings: RecorderSettings,
        _events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) -> Result<RecordingSession> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn stop_recording() -> Result<()> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }
}
