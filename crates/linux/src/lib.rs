#![cfg(target_os = "linux")]

mod desktop;
mod encoding;
mod pipeline;
mod portal;
mod x11;

use domain::{
    CaptureTarget, RecorderEngine, RecorderError, RecorderEvent, RecorderSettings,
    RecordingSession, Result,
};
use futures_util::StreamExt;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

pub use desktop::{check_desktop, list_targets};

pub(crate) fn backend(error: impl std::fmt::Display) -> RecorderError {
    RecorderError::Backend(error.to_string())
}

pub(crate) enum Command {
    Pause(mpsc::SyncSender<Result<()>>),
    Resume(mpsc::SyncSender<Result<()>>),
}

struct Active {
    commands: mpsc::SyncSender<Command>,
    stop: watch::Sender<bool>,
    worker: thread::JoinHandle<()>,
}

/// Rust owns lifecycle and buffer handles. GStreamer owns the native pixel path.
pub struct LinuxRecorder {
    events: mpsc::Sender<RecorderEvent>,
    active: Option<Active>,
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

    fn control(&self, command: impl FnOnce(mpsc::SyncSender<Result<()>>) -> Command) -> Result<()> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| backend("no active recording"))?;
        let (tx, rx) = mpsc::sync_channel(1);
        active.commands.try_send(command(tx)).map_err(backend)?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(backend)?
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
        if let Some(active) = &self.active {
            if !active.worker.is_finished() {
                return Err(backend("recording is already active"));
            }
        }
        if let Some(active) = self.active.take() {
            let _ = active.worker.join();
        }
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
        let events = self.events.clone();
        let completion_events = events.clone();
        let worker_session = session.clone();
        let (commands, receiver) = mpsc::sync_channel(1);
        let (stop, mut stopped) = watch::channel(false);
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("wrec-linux-capture".into())
            .spawn(move || {
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
                        );
                    }
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(backend)?;
                    runtime.block_on(async {
                        let capture =
                            portal::open(&target, settings.include_cursor, &mut stopped).await?;
                        let result = async {
                            let mut closed =
                                capture.session.receive_closed().await.map_err(backend)?;
                            let stream = portal::PipeWireStream {
                                fd: capture.stream.fd.try_clone().map_err(backend)?,
                                node: capture.stream.node,
                            };
                            let mut recording = tokio::task::spawn_blocking(move || {
                                pipeline::record(
                                    &pipeline::CaptureInput::PipeWire(stream),
                                    &worker_session,
                                    &settings,
                                    &events,
                                    receiver,
                                    &stopped,
                                )
                            });
                            tokio::select! {
                                result = &mut recording => result.map_err(backend)?,
                                _ = closed.next() => {
                                    let _ = worker_stop.send(true);
                                    recording.await.map_err(backend)?
                                }
                            }
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
            })
            .map_err(backend)?;
        self.active = Some(Active {
            commands,
            stop,
            worker,
        });
        Ok(session)
    }

    fn pause(&mut self) -> Result<()> {
        self.control(Command::Pause)
    }
    fn resume(&mut self) -> Result<()> {
        self.control(Command::Resume)
    }
    fn stop(&mut self) -> Result<()> {
        self.stop_requested = true;
        if let Some(active) = &self.active {
            let _ = active.stop.send(true);
        }
        Ok(())
    }
}

impl Drop for LinuxRecorder {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.stop.send(true);
            // The coordinator waits for finalization before normal shutdown.
            // A stuck native call must not turn destruction into an unbounded join.
            if active.worker.is_finished() {
                let _ = active.worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destruction_requests_stop_without_waiting_for_a_stuck_worker() {
        let (events, _) = mpsc::channel();
        let (commands, _) = mpsc::sync_channel(1);
        let (stop, stopped) = watch::channel(false);
        let (release, waiting) = mpsc::sync_channel::<()>(1);
        let worker = thread::spawn(move || {
            let _ = waiting.recv_timeout(Duration::from_secs(1));
        });
        let recorder = LinuxRecorder {
            events,
            active: Some(Active {
                commands,
                stop,
                worker,
            }),
            stop_requested: false,
        };
        let at = std::time::Instant::now();
        drop(recorder);
        let elapsed = at.elapsed();
        let _ = release.send(());
        assert!(
            elapsed < Duration::from_millis(200),
            "destructor waited {elapsed:?}"
        );
        assert!(*stopped.borrow());
    }

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
