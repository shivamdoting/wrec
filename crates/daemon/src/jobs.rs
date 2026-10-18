use control::{now_ms, AgentWarning, EventLevel, JobEvent, JobSnapshot, JobStatus, TargetSelector};
use domain::{CaptureTarget, RecorderMetrics, RecorderSettings};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(crate) struct JobRecord<E> {
    pub(crate) id: u64,
    pub(crate) status: JobStatus,
    pub(crate) selector: Option<TargetSelector>,
    pub(crate) target: CaptureTarget,
    pub(crate) settings: RecorderSettings,
    pub(crate) output_path: Option<PathBuf>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) warnings: Vec<AgentWarning>,
    pub(crate) events: Vec<JobEvent>,
    pub(crate) control: Option<Arc<Mutex<E>>>,
    pub(crate) created_at_ms: u64,
    pub(crate) updated_at_ms: u64,
    pub(crate) started_at_ms: Option<u64>,
    pub(crate) finished_at_ms: Option<u64>,
}

impl<E> JobRecord<E> {
    pub(crate) fn new(
        id: u64,
        selector: Option<TargetSelector>,
        target: CaptureTarget,
        settings: RecorderSettings,
        duration_ms: Option<u64>,
        warnings: Vec<AgentWarning>,
    ) -> Self {
        let now = now_ms();
        Self {
            id,
            status: JobStatus::Queued,
            selector,
            target,
            settings,
            output_path: None,
            duration_ms,
            warnings,
            events: Vec::new(),
            control: None,
            created_at_ms: now,
            updated_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
        }
    }

    pub(crate) fn snapshot(&self, queued_position: Option<usize>) -> JobSnapshot {
        JobSnapshot {
            id: self.id,
            status: self.status.clone(),
            selector: self.selector.clone(),
            target: Some(self.target.clone()),
            settings: Some(self.settings.clone()),
            output_path: self.output_path.clone(),
            queued_position,
            warnings: self.warnings.clone(),
            events: self.events.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
        }
    }

    pub(crate) fn mark_starting(&mut self) {
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(now_ms());
        }
        self.status = JobStatus::Starting;
        self.push_event(EventLevel::Info, "job starting");
    }

    pub(crate) fn mark_recording(&mut self) {
        if !self.is_terminal() {
            self.status = JobStatus::Recording;
            self.push_event(EventLevel::Info, "recording active");
        }
    }

    pub(crate) fn mark_finishing(&mut self) {
        if !self.is_terminal() {
            self.status = JobStatus::Finishing;
            self.push_event(EventLevel::Info, "stop requested");
        }
    }

    pub(crate) fn mark_cancelled(&mut self) {
        if !self.is_terminal() {
            self.status = JobStatus::Cancelled;
            self.finished_at_ms = Some(now_ms());
            self.control = None;
            self.push_event(EventLevel::Warning, "job cancelled");
        }
    }

    pub(crate) fn mark_failed(&mut self, message: impl Into<String>) {
        if !self.is_terminal() {
            self.status = JobStatus::Failed;
            self.finished_at_ms = Some(now_ms());
            self.control = None;
            self.push_event(EventLevel::Error, message);
        }
    }

    pub(crate) fn mark_completed(&mut self, status: impl Into<String>) {
        if !self.is_terminal() {
            self.status = JobStatus::Completed;
            self.finished_at_ms = Some(now_ms());
            self.control = None;
            self.push_event(EventLevel::Info, status);
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }

    pub(crate) fn push_event(&mut self, level: EventLevel, message: impl Into<String>) {
        let event = JobEvent {
            timestamp_ms: now_ms(),
            level,
            message: message.into(),
            metrics: None,
        };
        self.events.push(event);
        self.updated_at_ms = now_ms();
    }

    pub(crate) fn push_metrics(&mut self, metrics: RecorderMetrics) {
        let event = JobEvent {
            timestamp_ms: now_ms(),
            level: EventLevel::Info,
            message: format!(
                "{}s  {} bytes  {:.2} Mbps",
                metrics.elapsed_secs, metrics.output_bytes, metrics.estimated_bitrate_mbps
            ),
            metrics: Some(metrics),
        };
        // Metrics are a live snapshot, not an audit log. Replacing the
        // previous metrics event keeps job.show payloads and daemon memory
        // constant during long recordings while SQLite retains the full
        // time-series separately.
        if let Some(previous) = self
            .events
            .last_mut()
            .filter(|event| event.metrics.is_some())
        {
            *previous = event;
        } else {
            self.events.push(event);
        }
        self.updated_at_ms = now_ms();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, isolate_env};
    use domain::CaptureSourceKind;

    fn job() -> JobRecord<()> {
        JobRecord::new(
            7,
            None,
            CaptureTarget {
                id: 1,
                name: "Display".into(),
                kind: CaptureSourceKind::Display,
            },
            RecorderSettings::default(),
            None,
            Vec::new(),
        )
    }

    #[test]
    fn new_job_starts_queued_without_timestamps_or_events() {
        let _guard = env_lock();
        isolate_env();
        let job = job();

        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.started_at_ms, None);
        assert_eq!(job.finished_at_ms, None);
        assert!(job.events.is_empty());
        assert!(!job.is_terminal());
    }

    #[test]
    fn mark_starting_keeps_an_existing_start_time() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();
        job.started_at_ms = Some(123);

        job.mark_starting();

        assert_eq!(job.status, JobStatus::Starting);
        assert_eq!(job.started_at_ms, Some(123));
    }

    #[test]
    fn stop_flow_reaches_completed_and_drops_control() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();
        job.control = Some(Arc::new(Mutex::new(())));

        job.mark_starting();
        job.mark_recording();
        assert_eq!(job.status, JobStatus::Recording);

        job.mark_finishing();
        assert_eq!(job.status, JobStatus::Finishing);

        job.mark_completed("capture engine exited: 0");
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.is_terminal());
        assert!(job.finished_at_ms.is_some());
        assert!(job.control.is_none());
    }

    #[test]
    fn terminal_job_ignores_further_transitions() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();
        job.mark_completed("done");
        let events_before = job.events.len();

        job.mark_recording();
        job.mark_finishing();
        job.mark_failed("boom");
        job.mark_cancelled();

        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.events.len(), events_before);
    }

    #[test]
    fn cancelled_job_records_a_warning_event() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();

        job.mark_cancelled();

        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.is_terminal());
        assert!(job.finished_at_ms.is_some());
        assert!(matches!(
            job.events.last().unwrap().level,
            EventLevel::Warning
        ));
    }

    #[test]
    fn failed_job_records_the_failure_message_as_an_error_event() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();

        job.mark_failed("engine exploded");

        assert_eq!(job.status, JobStatus::Failed);
        let event = job.events.last().unwrap();
        assert!(matches!(event.level, EventLevel::Error));
        assert_eq!(event.message, "engine exploded");
    }

    #[test]
    fn push_event_touches_updated_at() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();
        job.updated_at_ms = 0;

        job.push_event(EventLevel::Info, "hello");

        assert!(job.updated_at_ms > 0);
        assert_eq!(job.events.last().unwrap().message, "hello");
    }

    #[test]
    fn push_metrics_attaches_metrics_to_the_event() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();

        job.push_metrics(RecorderMetrics {
            elapsed_secs: 3,
            output_bytes: 1024,
            estimated_bitrate_mbps: 1.5,
            frames: None,
            dropped_frames: None,
        });

        let event = job.events.last().unwrap();
        assert_eq!(event.message, "3s  1024 bytes  1.50 Mbps");
        assert_eq!(event.metrics.as_ref().unwrap().output_bytes, 1024);
    }

    #[test]
    fn consecutive_metrics_replace_the_live_snapshot() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();

        for elapsed_secs in 1..=10_000 {
            job.push_metrics(RecorderMetrics {
                elapsed_secs,
                output_bytes: elapsed_secs * 1024,
                estimated_bitrate_mbps: 1.5,
                frames: Some(elapsed_secs * 30),
                dropped_frames: Some(0),
            });
        }

        assert_eq!(job.events.len(), 1);
        let metrics = job.events[0].metrics.as_ref().unwrap();
        assert_eq!(metrics.elapsed_secs, 10_000);
        assert_eq!(metrics.output_bytes, 10_240_000);
    }

    #[test]
    fn lifecycle_events_preserve_the_latest_metrics_snapshot() {
        let _guard = env_lock();
        isolate_env();
        let mut job = job();

        let metrics = || RecorderMetrics {
            elapsed_secs: 1,
            output_bytes: 1024,
            estimated_bitrate_mbps: 1.5,
            frames: Some(30),
            dropped_frames: Some(0),
        };
        job.push_metrics(metrics());
        job.push_event(EventLevel::Info, "paused");
        job.push_metrics(metrics());
        job.push_metrics(metrics());

        assert_eq!(job.events.len(), 3);
        assert!(job.events[0].metrics.is_some());
        assert_eq!(job.events[1].message, "paused");
        assert!(job.events[2].metrics.is_some());
    }

    #[test]
    fn snapshot_carries_identity_and_queue_position() {
        let _guard = env_lock();
        isolate_env();
        let job = job();

        let snapshot = job.snapshot(Some(3));

        assert_eq!(snapshot.id, 7);
        assert_eq!(snapshot.status, JobStatus::Queued);
        assert_eq!(snapshot.queued_position, Some(3));
        assert_eq!(snapshot.target.unwrap().id, 1);
    }
}
