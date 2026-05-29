use wrec_core::{
    CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderError, RecorderMetrics,
    RecorderSettings, RecordingSession, Result, ScreenRecordingPermissionStatus,
};

static LAST_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    Starting {
        session_id: u64,
        target: CaptureTarget,
        settings: RecorderSettings,
        output_path: std::path::PathBuf,
    },
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

    pub fn screen_recording_permission_status(&self) -> Result<ScreenRecordingPermissionStatus> {
        platform::screen_recording_permission_status()
    }

    pub fn request_screen_recording_permission(&self) -> Result<ScreenRecordingPermissionStatus> {
        platform::request_screen_recording_permission()
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
        let session_id = next_session_id();
        let output_path = recording_output_path(&settings);
        self.emit(RecorderEvent::Starting {
            session_id,
            target: target.clone(),
            settings: settings.clone(),
            output_path: output_path.clone(),
        });
        let session = match platform::start_recording(
            session_id,
            target,
            settings,
            output_path,
            self.events.clone(),
        ) {
            Ok(session) => session,
            Err(err) => {
                self.emit(RecorderEvent::Failed {
                    session_id: Some(session_id),
                    message: err.to_string(),
                });
                return Err(err);
            }
        };
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

    fn pause(&mut self) -> Result<()> {
        let session_id = self.active.as_ref().map(|session| session.id);
        self.emit_log(session_id, "pausing recording");
        platform::pause_recording()?;
        self.emit_log(session_id, "recording pause requested");
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        let session_id = self.active.as_ref().map(|session| session.id);
        self.emit_log(session_id, "resuming recording");
        platform::resume_recording()?;
        self.emit_log(session_id, "recording resume requested");
        Ok(())
    }
}

impl Drop for MacosRecorder {
    fn drop(&mut self) {
        let _ = platform::stop_recording();
    }
}

fn next_session_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros().min(u64::MAX as u128) as u64)
        .unwrap_or_default();

    loop {
        let current = LAST_SESSION_ID.load(std::sync::atomic::Ordering::Relaxed);
        let next = now_micros.max(current.saturating_add(1));
        if LAST_SESSION_ID
            .compare_exchange(
                current,
                next,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
        {
            return next;
        }
    }
}

fn recording_output_path(settings: &RecorderSettings) -> std::path::PathBuf {
    let filename = format!(
        "wrec-{}.{}",
        chrono_like_timestamp(),
        settings.output_format.file_extension()
    );
    settings.output_dir.join(filename)
}

fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    secs.to_string()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant};

    const START_TIMEOUT: Duration = Duration::from_secs(5);
    const STOP_TIMEOUT: Duration = Duration::from_secs(20);
    const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

    struct RecordingProcess {
        child: std::process::Child,
        metrics_running: Arc<AtomicBool>,
    }

    enum StartupSignal {
        Started,
        Failed(String),
    }

    static CHILD: OnceLock<Mutex<Option<RecordingProcess>>> = OnceLock::new();
    const HELPER_NAME: &str = "wrec-helper";

    pub fn screen_recording_permission_status() -> Result<ScreenRecordingPermissionStatus> {
        run_permission_command("--permission-status")
    }

    pub fn request_screen_recording_permission() -> Result<ScreenRecordingPermissionStatus> {
        run_permission_command("--request-permission")
    }

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        use std::process::Command;

        let output = Command::new(helper_path()?)
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
        output_path: std::path::PathBuf,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) -> Result<RecordingSession> {
        use std::process::{Command, Stdio};

        std::fs::create_dir_all(&settings.output_dir)
            .map_err(|err| RecorderError::Backend(err.to_string()))?;

        let helper = helper_path()?;
        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let mut active_child = child_slot.lock().unwrap();
        if active_child.is_some() {
            return Err(RecorderError::Backend("recording is already active".into()));
        }

        // Temporary v0 native bridge: run a compiled Swift helper that uses
        // ScreenCaptureKit + AVAssetWriter. The frame path stays inside
        // Apple's native stack; Rust never receives/copies pixels.
        let include_system_audio =
            settings.include_system_audio && settings.output_format.supports_audio();
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
            .arg(settings.resolution.as_arg())
            .arg(if include_system_audio {
                "true"
            } else {
                "false"
            })
            .arg(if settings.hide_wrec { "true" } else { "false" })
            .arg(settings.output_format.as_arg())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| RecorderError::Backend(format!("failed to start helper: {err}")))?;

        let metrics_running = Arc::new(AtomicBool::new(true));
        let metrics_events = events.clone();
        let stderr = child.stderr.take();
        let (startup_tx, startup_rx) = mpsc::channel();

        *active_child = Some(RecordingProcess {
            child,
            metrics_running: metrics_running.clone(),
        });
        drop(active_child);

        let Some(stderr) = stderr else {
            let _ = kill_active_child();
            return Err(RecorderError::Backend(
                "helper stderr was not available for startup handshake".into(),
            ));
        };

        std::thread::spawn(move || {
            forward_helper_stderr(session_id, stderr, events, Some(startup_tx));
        });

        match startup_rx.recv_timeout(START_TIMEOUT) {
            Ok(StartupSignal::Started) => {}
            Ok(StartupSignal::Failed(message)) => {
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(format!(
                    "recording helper failed to start: {message}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let status = kill_active_child();
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(format!(
                    "recording helper did not report start within {}s{status}",
                    START_TIMEOUT.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(
                    "recording helper startup channel closed before recording started".into(),
                ));
            }
        }

        spawn_metrics_thread(
            session_id,
            output_path.clone(),
            metrics_running,
            metrics_events,
        );

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

    pub fn pause_recording() -> Result<()> {
        write_active_child_command("pause")
    }

    pub fn resume_recording() -> Result<()> {
        write_active_child_command("resume")
    }

    fn write_active_child_command(command: &str) -> Result<()> {
        use std::io::Write;

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let mut child = child_slot.lock().unwrap();
        let Some(process) = child.as_mut() else {
            return Err(RecorderError::Backend("no active recording".into()));
        };
        let Some(stdin) = process.child.stdin.as_mut() else {
            return Err(RecorderError::Backend(
                "recording helper stdin is unavailable".into(),
            ));
        };

        stdin
            .write_all(format!("{command}\n").as_bytes())
            .map_err(|err| RecorderError::Backend(format!("failed to send {command}: {err}")))
    }

    fn forward_helper_stderr(
        session_id: u64,
        stderr: std::process::ChildStderr,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
        startup: Option<mpsc::Sender<StartupSignal>>,
    ) {
        let mut startup = startup;
        let mut did_start = false;
        let mut first_startup_failure = None;

        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            eprintln!("{line}");
            let is_recording_started = helper_line_is_recording_started(&line);
            let is_failure = helper_line_is_failure(&line);

            if is_failure && first_startup_failure.is_none() {
                first_startup_failure = Some(line.clone());
            }
            if is_recording_started {
                did_start = true;
                signal_startup(&mut startup, StartupSignal::Started);
            }

            emit(
                &events,
                if did_start && is_failure {
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
            signal_startup(
                &mut startup,
                StartupSignal::Failed("failed to inspect helper exit status".into()),
            );
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
            if !did_start {
                signal_startup(
                    &mut startup,
                    StartupSignal::Failed("helper stderr closed before recording started".into()),
                );
            }
            return;
        };
        *child = None;

        if !did_start {
            signal_startup(
                &mut startup,
                StartupSignal::Failed(first_startup_failure.unwrap_or_else(|| {
                    format!("helper exited before recording started: {status}")
                })),
            );
            return;
        }

        emit(
            &events,
            RecorderEvent::Exited {
                session_id,
                success: status.success(),
                status: status.to_string(),
            },
        );
    }

    fn signal_startup(startup: &mut Option<mpsc::Sender<StartupSignal>>, signal: StartupSignal) {
        if let Some(startup) = startup.take() {
            let _ = startup.send(signal);
        }
    }

    fn kill_active_child() -> String {
        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let Some(mut process) = child_slot.lock().ok().and_then(|mut child| child.take()) else {
            return String::new();
        };

        process.metrics_running.store(false, Ordering::Relaxed);
        let _ = process.child.kill();
        match process.child.wait() {
            Ok(status) => format!("; killed helper with {status}"),
            Err(err) => format!("; failed to wait for killed helper: {err}"),
        }
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
            || line.contains("Assertion failed")
            || line.contains("CGS_REQUIRE_INIT")
    }

    fn helper_line_is_recording_started(line: &str) -> bool {
        line.contains("wrec-helper: recording started")
    }

    fn is_permission_error(message: &str) -> bool {
        message.contains("permission denied") || message.contains("Screen Recording access")
    }

    fn run_permission_command(arg: &str) -> Result<ScreenRecordingPermissionStatus> {
        use std::process::Command;

        let output = Command::new(helper_path()?)
            .arg(arg)
            .output()
            .map_err(|err| {
                RecorderError::Backend(format!(
                    "failed to check screen recording permission: {err}"
                ))
            })?;

        if !output.status.success() {
            return Err(RecorderError::Backend(format!(
                "screen recording permission check failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        match String::from_utf8_lossy(&output.stdout).trim() {
            "granted" => Ok(ScreenRecordingPermissionStatus::Granted),
            "missing" => Ok(ScreenRecordingPermissionStatus::Missing),
            status => Err(RecorderError::Backend(format!(
                "unknown screen recording permission status: {status}"
            ))),
        }
    }

    fn helper_path() -> Result<PathBuf> {
        std::env::var_os("WREC_HELPER_PATH")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(packaged_helper_path)
            .or_else(cargo_helper_path)
            .ok_or_else(|| {
                RecorderError::Backend(format!(
                    "{HELPER_NAME} was not found next to the app executable or in Cargo build output"
                ))
            })
    }

    fn packaged_helper_path() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(sibling_helper_path)
    }

    fn sibling_helper_path(exe_path: &Path) -> Option<PathBuf> {
        exe_path
            .parent()
            .map(|dir| dir.join(HELPER_NAME))
            .filter(|path| path.is_file())
    }

    fn cargo_helper_path() -> Option<PathBuf> {
        option_env!("WREC_HELPER_PATH")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn sibling_helper_path_finds_packaged_helper() {
            let dir = std::env::temp_dir().join(format!("wrec-helper-test-{}", std::process::id()));
            let exe = dir.join("wrec");
            let helper = dir.join(HELPER_NAME);

            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&exe, "").unwrap();
            std::fs::write(&helper, "").unwrap();

            assert_eq!(sibling_helper_path(&exe), Some(helper.clone()));

            let _ = std::fs::remove_file(exe);
            let _ = std::fs::remove_file(helper);
            let _ = std::fs::remove_dir(dir);
        }

        #[test]
        fn sibling_helper_path_ignores_missing_helper() {
            let exe = PathBuf::from("/tmp/wrec-missing-helper/wrec");

            assert_eq!(sibling_helper_path(&exe), None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn screen_recording_permission_status() -> Result<ScreenRecordingPermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn request_screen_recording_permission() -> Result<ScreenRecordingPermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn start_recording(
        _session_id: u64,
        _target: CaptureTarget,
        _settings: RecorderSettings,
        _output_path: std::path::PathBuf,
        _events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) -> Result<RecordingSession> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn stop_recording() -> Result<()> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn pause_recording() -> Result<()> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn resume_recording() -> Result<()> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }
}
