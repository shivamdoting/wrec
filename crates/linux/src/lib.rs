#![cfg(target_os = "linux")]

mod desktop;
mod encoding;
mod pipeline;
mod portal;
mod worker;
mod x11;

use domain::{
    CaptureTarget, RecorderEngine, RecorderError, RecorderEvent, RecorderSettings,
    RecordingSession, Result,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub use desktop::{check_desktop, list_targets};
pub use worker::{run as run_capture_worker, ARGUMENT as CAPTURE_WORKER_ARGUMENT};

pub(crate) fn backend(error: impl std::fmt::Display) -> RecorderError {
    RecorderError::Backend(error.to_string())
}

pub(crate) enum Command {
    Pause(mpsc::SyncSender<Result<()>>),
    Resume(mpsc::SyncSender<Result<()>>),
}

/// Rust owns lifecycle and buffer handles. GStreamer owns the native pixel path.
pub struct LinuxRecorder {
    events: mpsc::Sender<RecorderEvent>,
    active: Option<worker::Worker>,
    stop_requested: bool,
}

impl LinuxRecorder {
    pub fn new(events: mpsc::Sender<RecorderEvent>) -> Self {
        Self {
            events,
            active: None,
            stop_requested: false,
        }
    }

    fn control(&mut self, request: worker::Request) -> Result<()> {
        self.active
            .as_mut()
            .ok_or_else(|| backend("no active recording"))?
            .control(request)
    }
}

impl RecorderEngine for LinuxRecorder {
    fn list_targets(&self) -> Result<Vec<CaptureTarget>> {
        list_targets()
    }

    fn start(
        &mut self,
        target: CaptureTarget,
        settings: RecorderSettings,
    ) -> Result<RecordingSession> {
        if self.active.is_none() && self.stop_requested {
            return Err(RecorderError::Cancelled);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| !active.finished())
        {
            return Err(backend("recording is already active"));
        }
        self.active = None;
        self.stop_requested = false;
        check_desktop()?;
        if desktop::detect()? == desktop::Desktop::Wayland {
            portal::validate_target(&target)?;
        }
        std::fs::create_dir_all(&settings.output_dir).map_err(backend)?;
        static LAST_ID: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(backend)?
            .as_micros() as u64;
        let previous = LAST_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                Some(now.max(last.saturating_add(1)))
            })
            .unwrap();
        let id = now.max(previous.saturating_add(1));
        let session = RecordingSession {
            id,
            output_path: settings.output_dir.join(format!("wrec-{id}.mov")),
        };
        self.active = Some(worker::Worker::spawn(
            Path::new(worker::PROGRAM),
            &[worker::ARGUMENT],
            session.clone(),
            target,
            settings,
            self.events.clone(),
            worker::EXIT_DEADLINE,
        )?);
        Ok(session)
    }

    fn pause(&mut self) -> Result<()> {
        self.control(worker::Request::Pause)
    }
    fn resume(&mut self) -> Result<()> {
        self.control(worker::Request::Resume)
    }
    fn stop(&mut self) -> Result<()> {
        self.stop_requested = true;
        if let Some(active) = &mut self.active {
            active.stop();
        }
        Ok(())
    }
}

pub(crate) fn capture(
    target: CaptureTarget,
    settings: RecorderSettings,
    session: RecordingSession,
    events: mpsc::Sender<RecorderEvent>,
    receiver: mpsc::Receiver<Command>,
    worker_stop: watch::Sender<bool>,
    stopping: Arc<dyn Fn() + Send + Sync>,
) {
    let mut stopped = worker_stop.subscribe();
    let id = session.id;
    let completion_events = events.clone();
    let worker_session = session;
    let _ = events.send(RecorderEvent::Starting {
        session_id: id,
        target: target.clone(),
        settings: settings.clone(),
        output_path: worker_session.output_path.clone(),
    });
    let result = (|| {
        if desktop::detect()? == desktop::Desktop::X11 {
            let (display, xid) = x11::source(&target)?;
            return pipeline::record(
                &pipeline::CaptureInput::X11 { display, xid },
                &worker_session,
                &settings,
                &events,
                receiver,
                &stopped,
                &*stopping,
            );
        }
        let runtime = portal::runtime()?;
        runtime.block_on(async {
            let capture =
                Arc::new(portal::open(&target, settings.include_cursor, &mut stopped).await?);
            let result = async {
                let mut endings = capture.endings().await?;
                let node_watch = portal::NodeWatch::start(capture.connect().await?).await;
                if let portal::NodeWatch::Unavailable(reason) = &node_watch {
                    let _ = completion_events.send(RecorderEvent::Log {
                        session_id: Some(id),
                        message: format!("capture-engine: removal of the screen-cast stream will not be detected: {reason}"),
                    });
                }
                let ending_events = completion_events.clone();
                let ending_stopping = stopping.clone();
                let worker_capture = capture.clone();
                let runtime = tokio::runtime::Handle::current();
                let mut recording = tokio::task::spawn_blocking(move || {
                    pipeline::record(
                        &pipeline::CaptureInput::PipeWire(Box::new(move || {
                            // Each retry needs a new protocol connection. Duplicating
                            // an already-used socket does not reset its remote state.
                            runtime.block_on(worker_capture.connect())
                        })),
                        &worker_session,
                        &settings,
                        &events,
                        receiver,
                        &stopped,
                        &*stopping,
                    )
                });
                let result = tokio::select! {
                    result = &mut recording => result.map_err(backend)?,
                    ending = endings.next() => {
                        if matches!(ending, portal::Ending::Closed) {
                            let _ = ending_events.send(RecorderEvent::Log {
                                session_id: Some(id),
                                message: "capture-engine: the desktop closed the screen-cast session; finalizing the recording".into(),
                            });
                            ending_stopping();
                            let _ = worker_stop.send(true);
                        }
                        recording.await.map_err(backend)?
                    }
                };
                drop(node_watch);
                result
            }
            .await;
            // Release the remote only after all native elements have stopped using its fd.
            capture.close().await;
            result
        })
    })();
    let (success, status) = match result {
        Ok(()) => (true, "recording finalized".to_string()),
        Err(RecorderError::Cancelled) => {
            let _ = completion_events.send(RecorderEvent::Cancelled { session_id: id });
            return;
        }
        Err(error) => (false, error.to_string()),
    };
    let _ = completion_events.send(RecorderEvent::Exited {
        session_id: id,
        success,
        status,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_before_start_preserves_cancellation() {
        let (events, _) = mpsc::channel();
        let mut recorder = LinuxRecorder::new(events);
        recorder.stop().unwrap();
        let target = CaptureTarget {
            id: 0,
            kind: domain::CaptureSourceKind::Display,
            name: "picker".into(),
        };
        assert!(matches!(
            recorder.start(target, RecorderSettings::default()),
            Err(RecorderError::Cancelled)
        ));
    }
}
