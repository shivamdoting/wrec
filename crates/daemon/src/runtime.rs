use control::AgentError;
use domain::{CaptureTarget, PermissionStatus, RecorderEngine, RecorderError, RecorderEvent};
use macos::MacosRecorder;
use std::sync::mpsc;

pub(crate) trait RecordingRuntime: Clone + Send + Sync + 'static {
    type Engine: RecorderEngine + Send + 'static;

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError>;
    fn screen_recording_permission_status(&self) -> Result<PermissionStatus, AgentError>;
    fn request_screen_recording_permission(&self) -> Result<PermissionStatus, AgentError>;
    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine;
}

#[derive(Clone, Default)]
pub(crate) struct MacosRuntime;

impl RecordingRuntime for MacosRuntime {
    type Engine = MacosRecorder;

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError> {
        let (tx, _rx) = mpsc::channel();
        MacosRecorder::new(tx).list_targets().map_err(|err| AgentError {
            code: "target_listing_failed".into(),
            message: err.to_string(),
            recoverable: true,
            next: format!("Run `wrec targets --json` again; if this repeats, check Screen Recording permission and {}.", control::daemon_log_path().display()),
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

    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine {
        MacosRecorder::new(events)
    }
}

fn permission_error(error: RecorderError) -> AgentError {
    match error {
        RecorderError::MissingScreenRecordingPermission => AgentError {
            code: "screen_recording_permission_missing".into(),
            message: "screen recording permission is not granted".into(),
            recoverable: true,
            next: "Grant Screen Recording permission, then retry.".into(),
        },
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
