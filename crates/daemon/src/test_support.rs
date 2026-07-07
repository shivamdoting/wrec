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
        }
    }

    pub(crate) fn list_calls(&self) -> u64 {
        self.list_calls.load(Ordering::Relaxed)
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
            .send(RecorderEvent::Log {
                session_id: Some(id),
                message: "recording started".into(),
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
