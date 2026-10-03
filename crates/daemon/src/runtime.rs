use control::AgentError;
use domain::{CaptureTarget, PermissionStatus, RecorderEngine, RecorderError, RecorderEvent};
#[cfg(target_os = "macos")]
use macos::MacosRecorder;
use std::sync::mpsc;

pub(crate) trait RecordingRuntime: Clone + Send + Sync + 'static {
    type Engine: RecorderEngine + Send + 'static;

    fn prepare_settings(
        &self,
        _settings: &mut domain::RecorderSettings,
        _warnings: &mut Vec<control::AgentWarning>,
    ) {
    }

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError>;
    fn screen_recording_permission_status(&self) -> Result<PermissionStatus, AgentError>;
    fn request_screen_recording_permission(&self) -> Result<PermissionStatus, AgentError>;
    fn microphone_permission_status(&self) -> Result<PermissionStatus, AgentError>;
    /// Pops the system microphone dialog when access was never asked for;
    /// returns immediately when access is already decided.
    fn request_microphone_permission(&self) -> Result<PermissionStatus, AgentError>;
    /// Opens System Settings at Privacy & Security > Microphone.
    fn open_microphone_settings(&self) -> Result<(), AgentError>;
    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine;
}

#[cfg(target_os = "macos")]
#[derive(Clone, Default)]
pub(crate) struct MacosRuntime;

#[cfg(target_os = "macos")]
impl RecordingRuntime for MacosRuntime {
    type Engine = MacosRecorder;

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError> {
        let (tx, _rx) = mpsc::channel();
        MacosRecorder::new(tx).list_targets().map_err(|err| {
            if matches!(err, RecorderError::MissingScreenRecordingPermission) {
                warn_screen_recording_permission_missing();
            }
            AgentError {
                code: "target_listing_failed".into(),
                message: err.to_string(),
                recoverable: true,
                next: format!("Run `wrec targets --json` again; if this repeats, check Screen Recording permission and {}.", control::daemon_log_path().display()),
            }
        })
    }

    fn screen_recording_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        let (tx, _rx) = mpsc::channel();
        MacosRecorder::new(tx)
            .screen_recording_permission_status()
            .map_err(permission_error)
    }

    fn request_screen_recording_permission(&self) -> Result<PermissionStatus, AgentError> {
        let (tx, _rx) = mpsc::channel();
        MacosRecorder::new(tx)
            .request_screen_recording_permission()
            .map_err(permission_error)
    }

    fn microphone_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        macos::microphone_permission_status().map_err(permission_error)
    }

    fn request_microphone_permission(&self) -> Result<PermissionStatus, AgentError> {
        macos::request_microphone_permission().map_err(permission_error)
    }

    fn open_microphone_settings(&self) -> Result<(), AgentError> {
        macos::open_microphone_privacy_settings().map_err(permission_error)
    }

    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine {
        MacosRecorder::new(events)
    }
}

#[cfg(any(target_os = "macos", test))]
fn permission_error(error: RecorderError) -> AgentError {
    match error {
        RecorderError::MissingScreenRecordingPermission => {
            warn_screen_recording_permission_missing();
            AgentError {
                code: "screen_recording_permission_missing".into(),
                message: "screen recording permission is not granted".into(),
                recoverable: true,
                next: "Grant Screen Recording permission, then retry.".into(),
            }
        }
        RecorderError::Backend(message) if message.contains("capture-engine") => AgentError {
            code: "capture_engine_missing".into(),
            message: format!("backend error: {message}"),
            recoverable: true,
            next: "Build the daemon through Cargo or install the full wrec runtime so daemon and capture-engine are present together.".into(),
        },
        error => AgentError {
            code: "screen_recording_permission_failed".into(),
            message: error.to_string(),
            recoverable: true,
            next: "Fix the backend error above, then retry the permission check.".into(),
        },
    }
}

/// Screen Recording TCC is granted to whatever process launched this daemon
/// (Wrec.app or the terminal running `wrec daemon start`), not to the daemon
/// binary itself, so a denial here otherwise leaves no trail in daemon.log
/// pointing anyone at the actual app to go re-approve.
#[cfg(any(target_os = "macos", test))]
fn warn_screen_recording_permission_missing() {
    tracing::warn!(
        "screen recording permission is not granted; grant it to the app that launched this \
         daemon (Wrec.app, or the terminal/shell running `wrec`), then retry"
    );
}

#[cfg(target_os = "linux")]
pub(crate) use LinuxRuntime as PlatformRuntime;
#[cfg(target_os = "macos")]
pub(crate) use MacosRuntime as PlatformRuntime;

#[cfg(target_os = "linux")]
#[derive(Clone, Default)]
pub(crate) struct LinuxRuntime;

#[cfg(target_os = "linux")]
impl RecordingRuntime for LinuxRuntime {
    type Engine = linux::LinuxRecorder;

    fn prepare_settings(
        &self,
        settings: &mut domain::RecorderSettings,
        warnings: &mut Vec<control::AgentWarning>,
    ) {
        if settings.hide_wrec || (settings.include_microphone && settings.show_mic_indicator) {
            warnings.push(control::AgentWarning {
                code: "linux_settings_unavailable".into(),
                message: "Linux does not provide Wrec-window exclusion or a custom microphone indicator; these settings are disabled for this job.".into(),
                next: "Use the desktop's sharing controls and sound settings.".into(),
            });
        }
        settings.hide_wrec = false;
        settings.show_mic_indicator = false;
    }

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError> {
        linux::list_targets().map_err(linux_error)
    }

    fn screen_recording_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        linux::check_desktop().map_err(linux_error)?;
        // The portal grants access per session when recording starts.
        Ok(PermissionStatus::Unknown)
    }

    fn request_screen_recording_permission(&self) -> Result<PermissionStatus, AgentError> {
        self.screen_recording_permission_status()
    }

    fn microphone_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        // Native PulseAudio clients have no separate TCC permission prompt.
        // The audio server checks access when pulsesrc connects.
        Ok(PermissionStatus::Granted)
    }

    fn request_microphone_permission(&self) -> Result<PermissionStatus, AgentError> {
        self.microphone_permission_status()
    }

    fn open_microphone_settings(&self) -> Result<(), AgentError> {
        Err(linux_error(RecorderError::Backend(
            "Select the microphone in your desktop's sound settings.".into(),
        )))
    }

    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine {
        linux::LinuxRecorder::new(events)
    }
}

#[cfg(target_os = "linux")]
fn linux_error(error: RecorderError) -> AgentError {
    AgentError {
        code: "linux_capture_unavailable".into(),
        message: error.to_string(),
        recoverable: true,
        next: "Run wrec inside a Wayland desktop with its ScreenCast portal, or an X11 desktop with DISPLAY set. See packaging/linux/README.md.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_permission_maps_to_permission_missing_code() {
        let error = permission_error(RecorderError::MissingScreenRecordingPermission);

        assert_eq!(error.code, "screen_recording_permission_missing");
        assert!(error.recoverable);
    }

    #[test]
    fn capture_engine_backend_errors_map_to_capture_engine_missing() {
        let error = permission_error(RecorderError::Backend(
            "capture-engine binary not found".into(),
        ));

        assert_eq!(error.code, "capture_engine_missing");
        assert!(error.message.contains("capture-engine binary not found"));
    }

    #[test]
    fn other_errors_map_to_permission_failed() {
        let error = permission_error(RecorderError::Backend("boom".into()));

        assert_eq!(error.code, "screen_recording_permission_failed");
        assert_eq!(error.message, "backend error: boom");
    }
}
