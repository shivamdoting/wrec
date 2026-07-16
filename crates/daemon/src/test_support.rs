use crate::runtime::RecordingRuntime;
use control::{now_ms, AgentError};
use domain::{
    CaptureSourceKind, CaptureTarget, PermissionStatus, RecorderEngine, RecorderError,
    RecorderEvent, RecorderSettings, RecordingSession, Result as RecorderResult,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, MutexGuard, PoisonError,
    },
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn isolate_env() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wrec-test-{}-{}-{}",
        std::process::id(),
        now_ms(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let home = dir.join("home");
    std::env::set_var("WREC_HOME", &home);
    std::env::set_var("WREC_DATA_DIR", dir.join("data"));
    home
}

#[derive(Clone)]
pub(crate) struct FakeRuntime {
    targets: Arc<Vec<CaptureTarget>>,
    next_session_id: Arc<AtomicU64>,
    list_calls: Arc<AtomicU64>,
    mic_status: Arc<Mutex<PermissionStatus>>,
    mic_request_result: Arc<Mutex<PermissionStatus>>,
    mic_requests: Arc<AtomicU64>,
    mic_settings_opens: Arc<AtomicU64>,
}

pub(crate) struct FakeEngine {
    events: mpsc::Sender<RecorderEvent>,
    next_session_id: Arc<AtomicU64>,
    active: Option<RecordingSession>,
}

impl FakeRuntime {
    pub(crate) fn new() -> Self {
        Self {
            targets: Arc::new(vec![CaptureTarget {
                id: 1,
                name: "Display".into(),
                kind: CaptureSourceKind::Display,
            }]),
            next_session_id: Arc::new(AtomicU64::new(100)),
            list_calls: Arc::new(AtomicU64::new(0)),
            mic_status: Arc::new(Mutex::new(PermissionStatus::Granted)),
            mic_request_result: Arc::new(Mutex::new(PermissionStatus::Granted)),
            mic_requests: Arc::new(AtomicU64::new(0)),
            mic_settings_opens: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn list_calls(&self) -> u64 {
        self.list_calls.load(Ordering::Relaxed)
    }

    /// `status` answers the preflight check; `request_result` answers the
    /// follow-up request (the fake stand-in for the system dialog).
    pub(crate) fn set_mic_permission(
        &self,
        status: PermissionStatus,
        request_result: PermissionStatus,
    ) {
        *self.mic_status.lock().unwrap() = status;
        *self.mic_request_result.lock().unwrap() = request_result;
    }

    pub(crate) fn mic_requests(&self) -> u64 {
        self.mic_requests.load(Ordering::Relaxed)
    }

    pub(crate) fn mic_settings_opens(&self) -> u64 {
        self.mic_settings_opens.load(Ordering::Relaxed)
    }
}

impl RecordingRuntime for FakeRuntime {
    type Engine = FakeEngine;

    fn list_targets(&self) -> Result<Vec<CaptureTarget>, AgentError> {
        self.list_calls.fetch_add(1, Ordering::Relaxed);
        Ok((*self.targets).clone())
    }

    fn screen_recording_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        Ok(PermissionStatus::Granted)
    }

    fn request_screen_recording_permission(&self) -> Result<PermissionStatus, AgentError> {
        Ok(PermissionStatus::Granted)
    }

    fn microphone_permission_status(&self) -> Result<PermissionStatus, AgentError> {
        Ok(*self.mic_status.lock().unwrap())
    }

    fn request_microphone_permission(&self) -> Result<PermissionStatus, AgentError> {
        self.mic_requests.fetch_add(1, Ordering::Relaxed);
        Ok(*self.mic_request_result.lock().unwrap())
    }

    fn open_microphone_settings(&self) -> Result<(), AgentError> {
        self.mic_settings_opens.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn new_engine(&self, events: mpsc::Sender<RecorderEvent>) -> Self::Engine {
        FakeEngine {
            events,
            next_session_id: self.next_session_id.clone(),
            active: None,
        }
    }
}

impl RecorderEngine for FakeEngine {
    fn list_targets(&self) -> RecorderResult<Vec<CaptureTarget>> {
        Ok(vec![CaptureTarget {
            id: 1,
            name: "Display".into(),
            kind: CaptureSourceKind::Display,
        }])
    }

    fn start(
        &mut self,
        target: CaptureTarget,
        settings: RecorderSettings,
    ) -> RecorderResult<RecordingSession> {
        let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let output_path = settings.output_dir.join(format!("fake-{id}.mov"));
        let session = RecordingSession { id, output_path };
        self.active = Some(session.clone());
        self.events
            .send(RecorderEvent::Starting {
                session_id: id,
                target,
                settings,
                output_path: session.output_path.clone(),
            })
            .unwrap();
        self.events
            .send(RecorderEvent::Started {
                session_id: id,
                dimensions: None,
            })
            .unwrap();
        Ok(session)
    }

    fn pause(&mut self) -> RecorderResult<()> {
        Ok(())
    }

    fn resume(&mut self) -> RecorderResult<()> {
        Ok(())
    }

    fn stop(&mut self) -> RecorderResult<()> {
        let session = self
            .active
            .take()
            .ok_or_else(|| RecorderError::Backend("no active fake session".into()))?;
        self.events
            .send(RecorderEvent::Log {
                session_id: Some(session.id),
                message: "stopping recording".into(),
            })
            .unwrap();
        self.events
            .send(RecorderEvent::Exited {
                session_id: session.id,
                success: true,
                status: "exit status: 0".into(),
            })
            .unwrap();
        Ok(())
    }
}
