use domain::{
    CaptureDimensions, CaptureSourceKind, CaptureTarget, PermissionStatus, RecorderEngine,
    RecorderError, RecorderEvent, RecorderMetrics, RecorderSettings, RecordingSession, Result,
};

static LAST_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

    pub fn screen_recording_permission_status(&self) -> Result<PermissionStatus> {
        platform::screen_recording_permission_status()
    }

    pub fn request_screen_recording_permission(&self) -> Result<PermissionStatus> {
        platform::request_screen_recording_permission()
    }
}

pub fn microphone_permission_status() -> Result<PermissionStatus> {
    platform::microphone_permission_status()
}

pub fn request_microphone_permission() -> Result<PermissionStatus> {
    platform::request_microphone_permission()
}

pub fn open_microphone_privacy_settings() -> Result<()> {
    platform::open_microphone_privacy_settings()
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
        if self.owns_active_session() {
            let _ = platform::stop_recording();
        }
    }
}

impl MacosRecorder {
    fn owns_active_session(&self) -> bool {
        self.active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_recorder_does_not_own_an_active_session() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let recorder = MacosRecorder::new(tx);

        assert!(!recorder.owns_active_session());
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
    let filename = format!("wrec-{}.mov", chrono_like_timestamp());
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
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::sync::{mpsc, Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    const CAPTURE_ENGINE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
    // The mic request blocks on the macOS permission dialog until the user answers.
    const MIC_PERMISSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
    const START_TIMEOUT: Duration = Duration::from_secs(5);
    const STOP_TIMEOUT: Duration = Duration::from_secs(20);
    const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

    struct RecordingProcess {
        child: std::process::Child,
        session_id: u64,
        events: Option<mpsc::Sender<RecorderEvent>>,
    }

    enum StartupSignal {
        Started,
        Failed(String),
    }

    static CHILD: OnceLock<Mutex<Option<RecordingProcess>>> = OnceLock::new();
    const CAPTURE_ENGINE_NAME: &str = "capture-engine";

    // BSD sysexits(3) code emitted by the capture engine; keep in sync with
    // the Exit constants in native/capture_engine.swift.
    const EX_NOPERM: i32 = 77;

    pub fn screen_recording_permission_status() -> Result<PermissionStatus> {
        run_permission_command(
            "--permission-status",
            "screen recording permission check",
            CAPTURE_ENGINE_COMMAND_TIMEOUT,
        )
    }

    pub fn request_screen_recording_permission() -> Result<PermissionStatus> {
        run_permission_command(
            "--request-permission",
            "screen recording permission request",
            CAPTURE_ENGINE_COMMAND_TIMEOUT,
        )
    }

    pub fn microphone_permission_status() -> Result<PermissionStatus> {
        run_permission_command(
            "--mic-permission-status",
            "microphone permission check",
            CAPTURE_ENGINE_COMMAND_TIMEOUT,
        )
    }

    pub fn request_microphone_permission() -> Result<PermissionStatus> {
        run_permission_command(
            "--request-mic-permission",
            "microphone permission request",
            MIC_PERMISSION_REQUEST_TIMEOUT,
        )
    }

    /// macOS never re-shows the microphone dialog after a denial, so the
    /// closest thing to asking again is putting the user on the exact
    /// settings pane where access is granted.
    pub fn open_microphone_privacy_settings() -> Result<()> {
        let status = Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|err| {
                RecorderError::Backend(format!("failed to open System Settings: {err}"))
            })?;
        if !status.success() {
            return Err(RecorderError::Backend(format!(
                "failed to open System Settings: open exited with {status}"
            )));
        }
        Ok(())
    }

    pub fn list_targets() -> Result<Vec<CaptureTarget>> {
        let output = run_capture_engine_command(
            &["--list"],
            "target listing",
            CAPTURE_ENGINE_COMMAND_TIMEOUT,
        )?;

        if !output.status.success() {
            if output.status.code() == Some(EX_NOPERM) {
                return Err(RecorderError::MissingScreenRecordingPermission);
            }
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
        session_id: u64,
        target: CaptureTarget,
        settings: RecorderSettings,
        output_path: std::path::PathBuf,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
    ) -> Result<RecordingSession> {
        std::fs::create_dir_all(&settings.output_dir)
            .map_err(|err| RecorderError::Backend(err.to_string()))?;

        let capture_engine = capture_engine_path()?;
        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let mut active_child = child_slot.lock().unwrap();
        if active_child.is_some() {
            return Err(RecorderError::Backend("recording is already active".into()));
        }

        // Temporary v0 native bridge: run a compiled Swift capture engine that uses
        // ScreenCaptureKit + AVAssetWriter. The frame path stays inside
        // Apple's native stack; Rust never receives/copies pixels.
        let mut child = Command::new(capture_engine)
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
            .arg(if settings.include_system_audio {
                "true"
            } else {
                "false"
            })
            .arg(if settings.hide_wrec { "true" } else { "false" })
            .arg(if settings.include_microphone {
                "true"
            } else {
                "false"
            })
            .arg(if settings.show_mic_indicator {
                "true"
            } else {
                "false"
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                RecorderError::Backend(format!("failed to start capture engine: {err}"))
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (startup_tx, startup_rx) = mpsc::channel();

        *active_child = Some(RecordingProcess {
            child,
            session_id,
            events: events.clone(),
        });
        drop(active_child);

        let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
            let _ = kill_active_child();
            return Err(RecorderError::Backend(
                "capture engine stdio was not available for startup handshake".into(),
            ));
        };

        let recent_stderr = Arc::new(Mutex::new(VecDeque::new()));
        let log_events = events.clone();
        let log_buffer = recent_stderr.clone();
        std::thread::spawn(move || {
            forward_capture_engine_stderr(session_id, stderr, log_events, log_buffer);
        });
        let events_output_path = output_path.clone();
        std::thread::spawn(move || {
            handle_capture_engine_events(
                session_id,
                stdout,
                events,
                startup_tx,
                recent_stderr,
                events_output_path,
            );
        });

        match startup_rx.recv_timeout(START_TIMEOUT) {
            Ok(StartupSignal::Started) => {}
            Ok(StartupSignal::Failed(message)) => {
                let _ = kill_active_child();
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(format!(
                    "capture engine failed to start recording: {message}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let status = kill_active_child();
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(format!(
                    "capture engine did not report start within {}s{status}",
                    START_TIMEOUT.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = std::fs::remove_file(&output_path);
                return Err(RecorderError::Backend(
                    "capture engine startup channel closed before recording started".into(),
                ));
            }
        }

        tracing::info!(?target, ?settings, ?output_path, "started capture engine");
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
        let status = loop {
            let stopped = match process.child.try_wait() {
                Ok(stopped) => stopped,
                Err(err) => {
                    let message = format!("failed polling capture engine: {err}");
                    emit_failed(&process.events, Some(process.session_id), message.clone());
                    return Err(RecorderError::Backend(message));
                }
            };

            if let Some(status) = stopped {
                break status;
            }

            if started_waiting.elapsed() >= STOP_TIMEOUT {
                let _ = process.child.kill();
                let status = match process.child.wait() {
                    Ok(status) => status,
                    Err(err) => {
                        let message = format!("failed killing stuck capture engine: {err}");
                        emit_failed(&process.events, Some(process.session_id), message.clone());
                        return Err(RecorderError::Backend(message));
                    }
                };
                emit_exited(&process.events, process.session_id, &status);
                return Err(RecorderError::Backend(format!(
                    "capture engine did not stop recording within {}s and was killed with {status}",
                    STOP_TIMEOUT.as_secs()
                )));
            }

            std::thread::sleep(STOP_POLL_INTERVAL);
        };
        emit_exited(&process.events, process.session_id, &status);
        if !status.success() {
            return Err(RecorderError::Backend(format!(
                "capture engine exited with {status}"
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
                "capture engine stdin is unavailable".into(),
            ));
        };

        stdin
            .write_all(format!("{command}\n").as_bytes())
            .map_err(|err| RecorderError::Backend(format!("failed to send {command}: {err}")))
    }

    const RECENT_STDERR_LINES: usize = 8;

    /// stderr is the capture engine's human diagnostic channel. Lines are
    /// forwarded as logs and buffered for crash reports, never parsed for state.
    fn forward_capture_engine_stderr(
        session_id: u64,
        stderr: std::process::ChildStderr,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
        recent: Arc<Mutex<VecDeque<String>>>,
    ) {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            tracing::debug!(target: "capture_engine", "{line}");
            if let Ok(mut recent) = recent.lock() {
                if recent.len() == RECENT_STDERR_LINES {
                    recent.pop_front();
                }
                recent.push_back(line.clone());
            }
            emit(
                &events,
                RecorderEvent::Log {
                    session_id: Some(session_id),
                    message: line,
                },
            );
        }
    }

    /// stdout is the capture engine's machine event channel (one JSON event per
    /// line). It carries the startup handshake and failure signals; process exit
    /// is detected when the channel closes.
    fn handle_capture_engine_events(
        session_id: u64,
        stdout: std::process::ChildStdout,
        events: Option<std::sync::mpsc::Sender<RecorderEvent>>,
        startup: mpsc::Sender<StartupSignal>,
        recent_stderr: Arc<Mutex<VecDeque<String>>>,
        output_path: PathBuf,
    ) {
        let mut startup = Some(startup);
        let mut did_start = false;
        let mut last_failure: Option<String> = None;

        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            match parse_engine_event(&line) {
                Some(EngineEvent::Started { dimensions }) => {
                    did_start = true;
                    signal_startup(&mut startup, StartupSignal::Started);
                    emit(
                        &events,
                        RecorderEvent::Started {
                            session_id,
                            dimensions,
                        },
                    );
                }
                Some(EngineEvent::Metrics {
                    elapsed_secs,
                    frames,
                    dropped_frames,
                }) => {
                    let output_bytes = std::fs::metadata(&output_path)
                        .map(|metadata| metadata.len())
                        .unwrap_or_default();
                    let estimated_bitrate_mbps = if elapsed_secs > 0 {
                        output_bytes as f32 * 8. / elapsed_secs as f32 / 1_000_000.
                    } else {
                        0.
                    };
                    emit(
                        &events,
                        RecorderEvent::Metrics {
                            session_id,
                            metrics: RecorderMetrics {
                                elapsed_secs,
                                output_bytes,
                                estimated_bitrate_mbps,
                                frames,
                                dropped_frames,
                            },
                        },
                    );
                }
                Some(EngineEvent::Failed { message }) => {
                    if did_start {
                        emit(
                            &events,
                            RecorderEvent::Failed {
                                session_id: Some(session_id),
                                message,
                            },
                        );
                    } else {
                        last_failure = Some(message.clone());
                        signal_startup(&mut startup, StartupSignal::Failed(message));
                    }
                }
                Some(EngineEvent::Unknown) | None => {
                    tracing::warn!(target: "capture_engine", "unrecognized engine event: {line}");
                }
            }
        }

        let stderr_context = || {
            let lines = recent_stderr
                .lock()
                .map(|recent| recent.iter().cloned().collect::<Vec<_>>().join("; "))
                .unwrap_or_default();
            if lines.is_empty() {
                String::new()
            } else {
                format!(" (recent engine output: {lines})")
            }
        };

        let child_slot = CHILD.get_or_init(|| Mutex::new(None));
        let taken = match child_slot.lock() {
            Ok(mut child) => child.take(),
            Err(_) => {
                signal_startup(
                    &mut startup,
                    StartupSignal::Failed("failed to inspect capture engine exit status".into()),
                );
                return;
            }
        };
        let Some(mut process) = taken else {
            // stop_recording owns the process and reports its exit itself.
            if !did_start {
                signal_startup(
                    &mut startup,
                    StartupSignal::Failed(format!(
                        "capture engine event channel closed before recording started{}",
                        stderr_context()
                    )),
                );
            }
            return;
        };

        // A closed event channel means the engine exited or is unusable. Kill
        // is a no-op for a dead process, and a blocking wait cannot miss the
        // exit the way a single try_wait sample can (pipe EOF is observable
        // just before the process becomes reapable).
        let _ = process.child.kill();
        let status = match process.child.wait() {
            Ok(status) => status,
            Err(err) => {
                let message = format!("failed to reap capture engine: {err}");
                if !did_start {
                    signal_startup(&mut startup, StartupSignal::Failed(message));
                } else {
                    emit(
                        &events,
                        RecorderEvent::Failed {
                            session_id: Some(session_id),
                            message,
                        },
                    );
                }
                return;
            }
        };

        if !did_start {
            signal_startup(
                &mut startup,
                StartupSignal::Failed(last_failure.unwrap_or_else(|| {
                    format!(
                        "capture engine exited before recording started: {status}{}",
                        stderr_context()
                    )
                })),
            );
            return;
        }

        emit_exited(&events, session_id, &status);
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

        let _ = process.child.kill();
        match process.child.wait() {
            Ok(status) => format!("; killed capture engine with {status}"),
            Err(err) => format!("; failed to wait for killed capture engine: {err}"),
        }
    }

    fn emit(events: &Option<std::sync::mpsc::Sender<RecorderEvent>>, event: RecorderEvent) {
        if let Some(events) = events {
            let _ = events.send(event);
        }
    }

    fn emit_failed(
        events: &Option<mpsc::Sender<RecorderEvent>>,
        session_id: Option<u64>,
        message: String,
    ) {
        emit(
            events,
            RecorderEvent::Failed {
                session_id,
                message,
            },
        );
    }

    fn emit_exited(
        events: &Option<mpsc::Sender<RecorderEvent>>,
        session_id: u64,
        status: &std::process::ExitStatus,
    ) {
        emit(
            events,
            RecorderEvent::Exited {
                session_id,
                success: status.success(),
                status: status.to_string(),
            },
        );
    }

    /// Wire format of the capture engine's stdout event channel. Keep in sync
    /// with `emitEvent` calls in native/capture_engine.swift.
    #[derive(Debug, serde::Deserialize)]
    #[serde(tag = "event", rename_all = "snake_case")]
    enum RawEngineEvent {
        Started {
            native_width: Option<i64>,
            native_height: Option<i64>,
            output_width: Option<i64>,
            output_height: Option<i64>,
        },
        Metrics {
            elapsed: Option<u64>,
            frames: Option<u64>,
            dropped: Option<u64>,
        },
        Failed {
            message: String,
        },
        #[serde(other)]
        Unknown,
    }

    #[derive(Debug, PartialEq)]
    enum EngineEvent {
        Started {
            dimensions: Option<CaptureDimensions>,
        },
        Metrics {
            elapsed_secs: u64,
            frames: Option<u64>,
            dropped_frames: Option<u64>,
        },
        Failed {
            message: String,
        },
        Unknown,
    }

    fn parse_engine_event(line: &str) -> Option<EngineEvent> {
        Some(match serde_json::from_str::<RawEngineEvent>(line).ok()? {
            RawEngineEvent::Started {
                native_width,
                native_height,
                output_width,
                output_height,
            } => EngineEvent::Started {
                dimensions: match (native_width, native_height, output_width, output_height) {
                    (
                        Some(native_width),
                        Some(native_height),
                        Some(output_width),
                        Some(output_height),
                    ) => Some(CaptureDimensions {
                        native_width,
                        native_height,
                        output_width,
                        output_height,
                    }),
                    _ => None,
                },
            },
            RawEngineEvent::Metrics {
                elapsed,
                frames,
                dropped,
            } => EngineEvent::Metrics {
                elapsed_secs: elapsed.unwrap_or_default(),
                frames,
                dropped_frames: dropped,
            },
            RawEngineEvent::Failed { message } => EngineEvent::Failed { message },
            RawEngineEvent::Unknown => EngineEvent::Unknown,
        })
    }

    fn run_permission_command(
        arg: &str,
        label: &str,
        timeout: Duration,
    ) -> Result<PermissionStatus> {
        let output = run_capture_engine_command(&[arg], label, timeout)?;

        if !output.status.success() {
            return Err(RecorderError::Backend(format!(
                "{label} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        match String::from_utf8_lossy(&output.stdout).trim() {
            "granted" => Ok(PermissionStatus::Granted),
            "missing" => Ok(PermissionStatus::Missing),
            "unknown" => Ok(PermissionStatus::Unknown),
            status => Err(RecorderError::Backend(format!(
                "unknown {label} status: {status}"
            ))),
        }
    }

    fn run_capture_engine_command(args: &[&str], label: &str, timeout: Duration) -> Result<Output> {
        let mut child = Command::new(capture_engine_path()?)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| RecorderError::Backend(format!("{label} failed to start: {err}")))?;
        let started_at = Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    return child.wait_with_output().map_err(|err| {
                        RecorderError::Backend(format!(
                            "{label} failed to read capture engine output: {err}"
                        ))
                    });
                }
                Ok(None) if started_at.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RecorderError::Backend(format!(
                        "{label} timed out after {}s; killed capture engine. Run `wrec targets --json` again, and if this repeats, restart Wrec/Terminal and verify Screen Recording permission.",
                        timeout.as_secs()
                    )));
                }
                Ok(None) => std::thread::sleep(STOP_POLL_INTERVAL),
                Err(err) => {
                    return Err(RecorderError::Backend(format!(
                        "{label} failed while waiting for capture engine: {err}"
                    )));
                }
            }
        }
    }

    fn capture_engine_path() -> Result<PathBuf> {
        std::env::var_os("WREC_CAPTURE_ENGINE_PATH")
            .or_else(|| std::env::var_os("WREC_HELPER_PATH"))
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(packaged_capture_engine_path)
            .or_else(cargo_capture_engine_path)
            .ok_or_else(|| {
                RecorderError::Backend(format!(
                    "{CAPTURE_ENGINE_NAME} was not found next to the app executable or in Cargo build output"
                ))
            })
    }

    fn packaged_capture_engine_path() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(sibling_capture_engine_path)
    }

    fn sibling_capture_engine_path(exe_path: &Path) -> Option<PathBuf> {
        exe_path
            .parent()
            .map(|dir| dir.join(CAPTURE_ENGINE_NAME))
            .filter(|path| path.is_file())
    }

    fn cargo_capture_engine_path() -> Option<PathBuf> {
        option_env!("WREC_CAPTURE_ENGINE_PATH")
            .or(option_env!("WREC_HELPER_PATH"))
            .map(PathBuf::from)
            .filter(|path| path.is_file())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_started_event_with_dimensions() {
            let event = parse_engine_event(
                r#"{"event":"started","native_width":3024,"native_height":1964,"output_width":1512,"output_height":982}"#,
            );

            assert_eq!(
                event,
                Some(EngineEvent::Started {
                    dimensions: Some(CaptureDimensions {
                        native_width: 3024,
                        native_height: 1964,
                        output_width: 1512,
                        output_height: 982,
                    }),
                })
            );
        }

        #[test]
        fn parses_started_event_without_dimensions() {
            assert_eq!(
                parse_engine_event(r#"{"event":"started"}"#),
                Some(EngineEvent::Started { dimensions: None })
            );
        }

        #[test]
        fn parses_failed_event() {
            assert_eq!(
                parse_engine_event(r#"{"event":"failed","message":"no display found"}"#),
                Some(EngineEvent::Failed {
                    message: "no display found".into()
                })
            );
        }

        #[test]
        fn unknown_events_and_plain_text_are_not_failures() {
            assert_eq!(
                parse_engine_event(r#"{"event":"paused"}"#),
                Some(EngineEvent::Unknown)
            );
            assert_eq!(
                parse_engine_event("capture-engine: recording started"),
                None
            );
        }

        #[test]
        fn sibling_capture_engine_path_finds_packaged_capture_engine() {
            let dir =
                std::env::temp_dir().join(format!("capture-engine-test-{}", std::process::id()));
            let exe = dir.join("wrec");
            let capture_engine = dir.join(CAPTURE_ENGINE_NAME);

            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&exe, "").unwrap();
            std::fs::write(&capture_engine, "").unwrap();

            assert_eq!(
                sibling_capture_engine_path(&exe),
                Some(capture_engine.clone())
            );

            let _ = std::fs::remove_file(exe);
            let _ = std::fs::remove_file(capture_engine);
            let _ = std::fs::remove_dir(dir);
        }

        #[test]
        fn sibling_capture_engine_path_ignores_missing_capture_engine() {
            let exe = PathBuf::from("/tmp/wrec-missing-capture-engine/wrec");

            assert_eq!(sibling_capture_engine_path(&exe), None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn screen_recording_permission_status() -> Result<PermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn request_screen_recording_permission() -> Result<PermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn microphone_permission_status() -> Result<PermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn request_microphone_permission() -> Result<PermissionStatus> {
        Err(RecorderError::Backend("wrec only supports macOS".into()))
    }

    pub fn open_microphone_privacy_settings() -> Result<()> {
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
