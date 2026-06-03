use crate::{
    jobs::JobRecord,
    paths::{append_daemon_log, daemon_log_path, job_events_path, now_ms, socket_path, wrec_home},
    protocol::{AgentError, AgentWarning, EventLevel, JobStatus, StartRecordingParams},
    runtime::RecordingRuntime,
    target_resolution::{resolve_record_target, settings_for_target},
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock},
    thread,
    time::{Duration, Instant},
};
use wrec_backend::{build_settings_report, load_config, selected_target_id, BackendEvent};
use wrec_core::{CaptureTarget, RecorderEngine, RecorderEvent};

pub(crate) type SharedCoordinator<R> = Arc<Mutex<Coordinator<R>>>;

static TARGET_LIST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) struct Coordinator<R: RecordingRuntime> {
    runtime: R,
    backend: wrec_backend::WrecBackend,
    jobs: BTreeMap<u64, JobRecord<R::Engine>>,
    queue: VecDeque<u64>,
    target_cache: Vec<CaptureTarget>,
    active_job_id: Option<u64>,
    next_job_id: u64,
    shutdown_requested: bool,
}

impl<R: RecordingRuntime> Coordinator<R> {
    pub(crate) fn new(runtime: R) -> Self {
        Self {
            runtime,
            backend: wrec_backend::WrecBackend::open(),
            jobs: BTreeMap::new(),
            queue: VecDeque::new(),
            target_cache: Vec::new(),
            active_job_id: None,
            next_job_id: now_ms(),
            shutdown_requested: false,
        }
    }

    pub(crate) fn shutdown_requested(state: &SharedCoordinator<R>) -> bool {
        lock_state(state)
            .map(|state| state.shutdown_requested)
            .unwrap_or(true)
    }

    pub(crate) fn status(&self) -> Value {
        json!({
            "pid": std::process::id(),
            "home": wrec_home(),
            "socket": socket_path(),
            "daemon_log": daemon_log_path(),
            "job_events": job_events_path(),
            "active_job_id": self.active_job_id,
            "queued_jobs": self.queue.iter().copied().collect::<Vec<_>>(),
            "stopping": self.shutdown_requested,
        })
    }

    pub(crate) fn daemon_stop(state: SharedCoordinator<R>) -> Result<Value, AgentError> {
        let mut state = lock_state(&state)?;
        if let Some(active) = state.active_job_id {
            return Err(AgentError {
                code: "daemon_busy".into(),
                message: format!("Daemon cannot stop gracefully while job {active} is active."),
                recoverable: true,
                next: format!(
                    "Use `wrec job stop {active}`, wait for it to finish, then retry `wrec daemon stop`."
                ),
            });
        }
        if !state.queue.is_empty() {
            return Err(AgentError {
                code: "daemon_busy".into(),
                message: format!(
                    "Daemon cannot stop gracefully with {} queued job(s).",
                    state.queue.len()
                ),
                recoverable: true,
                next:
                    "Run `wrec jobs --json`, cancel queued jobs with `wrec job cancel <id>`, then retry `wrec daemon stop`."
                        .into(),
            });
        }

        state.shutdown_requested = true;
        append_daemon_log("shutdown requested");
        Ok(json!({
            "stopping": true,
            "home": wrec_home(),
            "socket": socket_path(),
            "daemon_log": daemon_log_path(),
        }))
    }

    pub(crate) fn targets_list(state: SharedCoordinator<R>) -> Result<Value, AgentError> {
        let targets = list_targets_with_cache(&state, true)?;
        Ok(json!({ "targets": targets }))
    }

    pub(crate) fn record_start(
        state: SharedCoordinator<R>,
        params: StartRecordingParams,
    ) -> Result<Value, AgentError> {
        if lock_state(&state)?.shutdown_requested {
            return Err(AgentError {
                code: "daemon_stopping".into(),
                message: "Daemon is stopping and is not accepting new recording jobs.".into(),
                recoverable: true,
                next: "Wait a moment, then run `wrec daemon start` and retry the recording.".into(),
            });
        }

        let (job, should_launch) = {
            let config = load_config();
            let overrides = (&params.options).into();
            let (settings, warning) = build_settings_report(&config.settings, &overrides);
            let warnings = warning
                .map(|message| AgentWarning {
                    code: "preset_limited".into(),
                    message,
                    next: "Use --quality high to allow native/60 FPS, or accept the effective capped settings.".into(),
                })
                .into_iter()
                .collect::<Vec<_>>();
            let targets = list_targets_with_cache(&state, true)?;
            let target = resolve_record_target(
                &targets,
                settings.source,
                params.selector.as_ref(),
                selected_target_id(&config, settings.source),
            )?;
            let settings = settings_for_target(settings, &target);

            let mut state = lock_state(&state)?;
            let id = state.allocate_job_id();
            let mut job = JobRecord::new(
                id,
                params.selector,
                target,
                settings,
                params.duration_ms,
                warnings,
            );

            let should_launch = state.active_job_id.is_none();
            if should_launch {
                job.mark_starting();
                state.active_job_id = Some(id);
            } else if params.queue {
                state.queue.push_back(id);
                job.push_event(
                    EventLevel::Info,
                    format!(
                        "job queued behind active job {}",
                        state.active_job_id.unwrap_or_default()
                    ),
                );
            } else {
                return Err(AgentError {
                    code: "recording_active".into(),
                    message: format!(
                        "Job {} is already active; this request was not queued.",
                        state.active_job_id.unwrap_or_default()
                    ),
                    recoverable: true,
                    next: "Retry with `--queue`, wait for the active job, or stop it with `wrec job stop <id>`.".into(),
                });
            }

            let snapshot = job.snapshot(state.queued_position(id));
            state.jobs.insert(id, job);
            append_daemon_log(format!("accepted job {id}"));
            (snapshot, should_launch)
        };

        if should_launch {
            launch_job(state.clone(), job.id);
        }

        Ok(json!({
            "job": job,
            "next": if should_launch {
                "Job is starting. Use `wrec job show <id> --json` to inspect it."
            } else {
                "Job is queued. Use `wrec jobs --json` to watch queue position."
            }
        }))
    }

    pub(crate) fn jobs_list(&self) -> Value {
        let jobs = self
            .jobs
            .values()
            .map(|job| job.snapshot(self.queued_position(job.id)))
            .collect::<Vec<_>>();
        json!({ "jobs": jobs, "active_job_id": self.active_job_id })
    }

    pub(crate) fn job_show(&self, id: u64) -> Result<Value, AgentError> {
        let job = self.jobs.get(&id).ok_or_else(|| missing_job_error(id))?;
        Ok(json!({ "job": job.snapshot(self.queued_position(id)) }))
    }

    pub(crate) fn job_logs(&self, id: u64) -> Result<Value, AgentError> {
        let job = self.jobs.get(&id).ok_or_else(|| missing_job_error(id))?;
        Ok(json!({ "job_id": id, "events": job.events }))
    }

    pub(crate) fn job_cancel(state: SharedCoordinator<R>, id: u64) -> Result<Value, AgentError> {
        let mut state = lock_state(&state)?;
        if state.active_job_id == Some(id) {
            return Err(AgentError {
                code: "job_active".into(),
                message: format!("Job {id} is active and cannot be cancelled as a queued job."),
                recoverable: true,
                next: format!("Use `wrec job stop {id}` to stop the active recording."),
            });
        }
        let Some(position) = state.queue.iter().position(|job_id| *job_id == id) else {
            return Err(missing_job_error(id));
        };
        state.queue.remove(position);
        let queued_position = state.queued_position(id);
        let job = state
            .jobs
            .get_mut(&id)
            .ok_or_else(|| missing_job_error(id))?;
        job.mark_cancelled();
        Ok(json!({ "job": job.snapshot(queued_position) }))
    }

    pub(crate) fn job_pause(state: SharedCoordinator<R>, id: u64) -> Result<Value, AgentError> {
        let control = {
            let mut state = lock_state(&state)?;
            let job = active_job_mut(&mut state, id)?;
            if job.status != JobStatus::Recording {
                return Err(job_state_error(
                    id,
                    "job_not_recording",
                    format!("Job {id} is {} and cannot be paused.", status_name(&job.status)),
                    "Wait until the job status is recording, or inspect it with `wrec job show <id> --json`.",
                ));
            }
            job.control.clone()
        };

        let Some(control) = control else {
            return Err(missing_job_control_error(id));
        };
        lock_control(&control, id)?
            .pause()
            .map_err(|err| record_control_error("job_pause_failed", id, err.to_string()))?;

        let mut state = lock_state(&state)?;
        let job = active_job_mut(&mut state, id)?;
        job.status = JobStatus::Paused;
        job.push_event(EventLevel::Info, "pause requested");
        Ok(json!({ "job": job.snapshot(None) }))
    }

    pub(crate) fn job_resume(state: SharedCoordinator<R>, id: u64) -> Result<Value, AgentError> {
        let control =
            {
                let mut state = lock_state(&state)?;
                let job = active_job_mut(&mut state, id)?;
                if job.status != JobStatus::Paused {
                    return Err(job_state_error(
                    id,
                    "job_not_paused",
                    format!("Job {id} is {} and cannot be resumed.", status_name(&job.status)),
                    "Pause the active job first, or inspect it with `wrec job show <id> --json`.",
                ));
                }
                job.control.clone()
            };

        let Some(control) = control else {
            return Err(missing_job_control_error(id));
        };
        lock_control(&control, id)?
            .resume()
            .map_err(|err| record_control_error("job_resume_failed", id, err.to_string()))?;

        let mut state = lock_state(&state)?;
        let job = active_job_mut(&mut state, id)?;
        job.status = JobStatus::Recording;
        job.push_event(EventLevel::Info, "resume requested");
        Ok(json!({ "job": job.snapshot(None) }))
    }

    pub(crate) fn job_stop(state: SharedCoordinator<R>, id: u64) -> Result<Value, AgentError> {
        let control = {
            let mut state = lock_state(&state)?;
            let job = active_job_mut(&mut state, id)?;
            let control = job
                .control
                .clone()
                .ok_or_else(|| missing_job_control_error(id))?;
            job.mark_finishing();
            control
        };

        if let Err(err) = lock_control(&control, id)?.stop() {
            let mut state = lock_state(&state)?;
            if let Ok(job) = active_job_mut(&mut state, id) {
                job.status = JobStatus::Recording;
                job.push_event(EventLevel::Error, format!("stop failed: {err}"));
            }
            return Err(record_control_error("job_stop_failed", id, err.to_string()));
        }

        let state = lock_state(&state)?;
        let job = state.jobs.get(&id).ok_or_else(|| missing_job_error(id))?;
        Ok(json!({ "job": job.snapshot(state.queued_position(id)) }))
    }

    fn allocate_job_id(&mut self) -> u64 {
        self.next_job_id = self.next_job_id.saturating_add(1);
        self.next_job_id
    }

    fn queued_position(&self, id: u64) -> Option<usize> {
        self.queue
            .iter()
            .position(|job_id| *job_id == id)
            .map(|index| index + 1)
    }
}

pub(crate) fn lock_state<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
) -> Result<MutexGuard<'_, Coordinator<R>>, AgentError> {
    state.lock().map_err(|_| AgentError {
        code: "daemon_state_poisoned".into(),
        message: "Daemon state lock was poisoned by an earlier internal failure.".into(),
        recoverable: true,
        next: "Stop active recordings if possible, then restart the daemon.".into(),
    })
}

fn list_targets_with_cache<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
    allow_active_cache: bool,
) -> Result<Vec<CaptureTarget>, AgentError> {
    if allow_active_cache {
        if let Some(targets) = {
            let state = lock_state(state)?;
            (state.active_job_id.is_some() && !state.target_cache.is_empty())
                .then(|| state.target_cache.clone())
        } {
            return Ok(targets);
        }
    }

    let _guard = target_list_lock().lock().map_err(|_| AgentError {
        code: "target_listing_lock_poisoned".into(),
        message: "Target listing lock was poisoned by an earlier internal failure.".into(),
        recoverable: true,
        next: "Restart the daemon and retry target listing.".into(),
    })?;

    if allow_active_cache {
        if let Some(targets) = {
            let state = lock_state(state)?;
            (state.active_job_id.is_some() && !state.target_cache.is_empty())
                .then(|| state.target_cache.clone())
        } {
            return Ok(targets);
        }
    }

    let runtime = lock_state(state)?.runtime.clone();
    let targets = runtime.list_targets()?;
    lock_state(state)?.target_cache = targets.clone();
    Ok(targets)
}

fn launch_job<R: RecordingRuntime>(state: SharedCoordinator<R>, job_id: u64) {
    let runtime = match lock_state(&state).map(|state| state.runtime.clone()) {
        Ok(runtime) => runtime,
        Err(err) => {
            append_daemon_log(format!("job {job_id} launch failed: {}", err.message));
            finish_job_failed(
                &state,
                job_id,
                format!("recording failed to launch: {}", err.message),
            );
            launch_next_queued_job(state.clone());
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(Mutex::new(runtime.new_engine(tx)));
    let job_parts = {
        let mut state = match lock_state(&state) {
            Ok(state) => state,
            Err(err) => {
                append_daemon_log(format!("job {job_id} launch failed: {}", err.message));
                finish_job_failed(
                    &state,
                    job_id,
                    format!("recording failed to launch: {}", err.message),
                );
                launch_next_queued_job(state.clone());
                return;
            }
        };
        let Some(job) = state.jobs.get_mut(&job_id) else {
            return;
        };
        if job.is_terminal() {
            return;
        }
        job.status = JobStatus::Starting;
        job.started_at_ms.get_or_insert_with(now_ms);
        job.control = Some(engine.clone());
        (
            job.target.clone(),
            job.settings.clone(),
            job.duration_ms,
            engine,
            rx,
        )
    };

    thread::spawn(move || {
        let (target, settings, duration_ms, engine, rx) = job_parts;
        run_job(
            state.clone(),
            job_id,
            target,
            settings,
            duration_ms,
            engine,
            rx,
        );
        launch_next_queued_job(state);
    });
}

fn run_job<R: RecordingRuntime>(
    state: SharedCoordinator<R>,
    job_id: u64,
    target: CaptureTarget,
    settings: wrec_core::RecorderSettings,
    duration_ms: Option<u64>,
    engine: Arc<Mutex<R::Engine>>,
    rx: mpsc::Receiver<RecorderEvent>,
) {
    append_daemon_log(format!("job {job_id} starting"));
    let start_result = match lock_control(&engine, job_id) {
        Ok(mut engine) => engine.start(target, settings),
        Err(err) => {
            finish_job_failed(&state, job_id, err.message);
            return;
        }
    };
    if let Err(err) = start_result {
        drain_recorder_events(&state, job_id, &rx);
        finish_job_failed(&state, job_id, format!("recording failed to start: {err}"));
        return;
    }

    if let Ok(mut state) = lock_state(&state) {
        if let Some(job) = state.jobs.get_mut(&job_id) {
            job.mark_recording();
        }
    }

    let started = Instant::now();
    let mut duration_stop_requested = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                let done = handle_recorder_event(&state, job_id, event);
                if done {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                finish_job_failed(&state, job_id, "recorder event channel closed");
                break;
            }
        }

        if let Some(duration_ms) = duration_ms {
            if !duration_stop_requested && started.elapsed() >= Duration::from_millis(duration_ms) {
                duration_stop_requested = true;
                append_job_message(
                    &state,
                    job_id,
                    EventLevel::Info,
                    "duration elapsed; stopping",
                );
                let _ = lock_control(&engine, job_id).and_then(|mut engine| {
                    engine.stop().map_err(|err| {
                        record_control_error("job_stop_failed", job_id, err.to_string())
                    })
                });
            }
        }
    }
}

fn drain_recorder_events<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
    job_id: u64,
    rx: &mpsc::Receiver<RecorderEvent>,
) {
    while let Ok(event) = rx.try_recv() {
        handle_recorder_event(state, job_id, event);
    }
}

fn handle_recorder_event<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
    job_id: u64,
    event: RecorderEvent,
) -> bool {
    let mut state = match lock_state(state) {
        Ok(state) => state,
        Err(err) => {
            append_daemon_log(format!("job {job_id} event failed: {}", err.message));
            return true;
        }
    };
    let backend_event = state.backend.handle_recorder_event(&event);
    let active_matches = state.active_job_id == Some(job_id);
    let job = match state.jobs.get_mut(&job_id) {
        Some(job) => job,
        None => return true,
    };
    if job.is_terminal() {
        if active_matches {
            state.active_job_id = None;
        }
        return true;
    }

    match backend_event {
        BackendEvent::Starting { output_path, .. } => {
            job.output_path = Some(output_path.clone());
            job.push_event(
                EventLevel::Info,
                format!("starting capture -> {}", output_path.display()),
            );
            false
        }
        BackendEvent::Log { message, .. } => {
            job.push_event(EventLevel::Info, message);
            false
        }
        BackendEvent::Metrics { metrics, .. } => {
            job.push_metrics(metrics);
            false
        }
        BackendEvent::Failed { message, .. } => {
            job.mark_failed(message);
            if active_matches {
                state.active_job_id = None;
            }
            true
        }
        BackendEvent::Exited {
            success,
            status,
            output_path,
            ..
        } => {
            job.output_path = output_path.or_else(|| job.output_path.clone());
            if success {
                job.mark_completed(format!("helper exited: {status}"));
            } else {
                job.mark_failed(format!("helper exited: {status}"));
            }
            if active_matches {
                state.active_job_id = None;
            }
            true
        }
    }
}

fn finish_job_failed<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
    job_id: u64,
    message: impl Into<String>,
) {
    let mut state = match state.lock() {
        Ok(state) => state,
        Err(err) => {
            append_daemon_log(format!(
                "job {job_id} fail handling recovered poisoned state"
            ));
            state.clear_poison();
            err.into_inner()
        }
    };
    if let Some(job) = state.jobs.get_mut(&job_id) {
        job.mark_failed(message);
    }
    if state.active_job_id == Some(job_id) {
        state.active_job_id = None;
    }
}

fn append_job_message<R: RecordingRuntime>(
    state: &SharedCoordinator<R>,
    job_id: u64,
    level: EventLevel,
    message: impl Into<String>,
) {
    if let Ok(mut state) = lock_state(state) {
        if let Some(job) = state.jobs.get_mut(&job_id) {
            job.push_event(level, message);
        }
    }
}

fn launch_next_queued_job<R: RecordingRuntime>(state: SharedCoordinator<R>) {
    let next_job = {
        let mut state = match lock_state(&state) {
            Ok(state) => state,
            Err(err) => {
                append_daemon_log(format!("queue launch failed: {}", err.message));
                return;
            }
        };
        if state.active_job_id.is_some() {
            None
        } else {
            let next = loop {
                let Some(job_id) = state.queue.pop_front() else {
                    break None;
                };
                if state
                    .jobs
                    .get(&job_id)
                    .is_some_and(|job| job.status == JobStatus::Queued)
                {
                    break Some(job_id);
                }
            };
            if let Some(job_id) = next {
                state.active_job_id = Some(job_id);
            }
            next
        }
    };

    if let Some(job_id) = next_job {
        launch_job(state, job_id);
    }
}

fn active_job_mut<R: RecordingRuntime>(
    state: &mut Coordinator<R>,
    id: u64,
) -> Result<&mut JobRecord<R::Engine>, AgentError> {
    if state.active_job_id != Some(id) {
        return Err(AgentError {
            code: "job_not_active".into(),
            message: format!("Job {id} is not the active recording."),
            recoverable: true,
            next: "Use `wrec jobs --json` to find the active job, or `wrec job cancel <id>` for queued jobs.".into(),
        });
    }
    state.jobs.get_mut(&id).ok_or_else(|| missing_job_error(id))
}

fn lock_control<E>(control: &Arc<Mutex<E>>, id: u64) -> Result<MutexGuard<'_, E>, AgentError> {
    control.lock().map_err(|_| AgentError {
        code: "job_control_poisoned".into(),
        message: format!("Job {id} recorder control lock was poisoned."),
        recoverable: true,
        next: format!("Inspect `wrec job show {id} --json`; if it is stuck, restart the daemon."),
    })
}

fn target_list_lock() -> &'static Mutex<()> {
    TARGET_LIST_LOCK.get_or_init(|| Mutex::new(()))
}

fn job_state_error(
    id: u64,
    code: &str,
    message: impl Into<String>,
    next: impl Into<String>,
) -> AgentError {
    AgentError {
        code: code.into(),
        message: message.into(),
        recoverable: true,
        next: next.into().replace("<id>", &id.to_string()),
    }
}

fn missing_job_control_error(id: u64) -> AgentError {
    AgentError {
        code: "job_control_missing".into(),
        message: format!("Job {id} does not have an active recorder handle."),
        recoverable: true,
        next: "Wait for the job to fail or inspect `wrec job show <id> --json`.".into(),
    }
}

fn record_control_error(code: &str, id: u64, message: String) -> AgentError {
    AgentError {
        code: code.into(),
        message,
        recoverable: true,
        next: format!(
            "Inspect `wrec job show {id} --json`; if the helper is stuck, restart the daemon."
        ),
    }
}

fn status_name(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Starting => "starting",
        JobStatus::Recording => "recording",
        JobStatus::Paused => "paused",
        JobStatus::Finishing => "finishing",
        JobStatus::Completed => "completed",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
    }
}

pub(crate) fn missing_job_error(id: u64) -> AgentError {
    AgentError {
        code: "job_not_found".into(),
        message: format!("No job with id {id} is known to this daemon."),
        recoverable: true,
        next: "Run `wrec jobs --json` to list jobs known to the current daemon.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{RecordingOptions, TargetSelector};
    use std::sync::atomic::{AtomicU64, Ordering};
    use wrec_core::{
        CaptureSourceKind, RecorderError, RecorderSettings, RecordingSession,
        Result as RecorderResult,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone)]
    struct FakeRuntime {
        targets: Arc<Vec<CaptureTarget>>,
        next_session_id: Arc<AtomicU64>,
        list_calls: Arc<AtomicU64>,
    }

    struct FakeEngine {
        events: mpsc::Sender<RecorderEvent>,
        next_session_id: Arc<AtomicU64>,
        active: Option<RecordingSession>,
    }

    impl FakeRuntime {
        fn new() -> Self {
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

        fn list_calls(&self) -> u64 {
            self.list_calls.load(Ordering::Relaxed)
        }
    }

    impl RecordingRuntime for FakeRuntime {
        type Engine = FakeEngine;

        fn list_targets(&self) -> std::result::Result<Vec<CaptureTarget>, AgentError> {
            self.list_calls.fetch_add(1, Ordering::Relaxed);
            Ok((*self.targets).clone())
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

    #[test]
    fn queued_job_launches_after_active_job_stops() {
        let _guard = ENV_LOCK.lock().unwrap();
        isolate_env();
        let state = Arc::new(Mutex::new(Coordinator::new(FakeRuntime::new())));
        let first = start_job(state.clone()).id;
        wait_for_status(&state, first, JobStatus::Recording);

        let second = start_job(state.clone()).id;
        assert_eq!(
            lock_state(&state)
                .unwrap()
                .jobs
                .get(&second)
                .unwrap()
                .status,
            JobStatus::Queued
        );

        Coordinator::job_stop(state.clone(), first).unwrap();
        wait_for_status(&state, first, JobStatus::Completed);
        wait_for_status(&state, second, JobStatus::Recording);

        Coordinator::job_stop(state.clone(), second).unwrap();
        wait_for_status(&state, second, JobStatus::Completed);
    }

    #[test]
    fn queued_job_can_be_cancelled() {
        let _guard = ENV_LOCK.lock().unwrap();
        isolate_env();
        let state = Arc::new(Mutex::new(Coordinator::new(FakeRuntime::new())));
        let first = start_job(state.clone()).id;
        wait_for_status(&state, first, JobStatus::Recording);
        let second = start_job(state.clone()).id;

        Coordinator::job_cancel(state.clone(), second).unwrap();
        assert_eq!(
            lock_state(&state)
                .unwrap()
                .jobs
                .get(&second)
                .unwrap()
                .status,
            JobStatus::Cancelled
        );

        Coordinator::job_stop(state.clone(), first).unwrap();
        wait_for_status(&state, first, JobStatus::Completed);
    }

    #[test]
    fn active_job_pause_resume_and_stop_are_state_checked() {
        let _guard = ENV_LOCK.lock().unwrap();
        isolate_env();
        let state = Arc::new(Mutex::new(Coordinator::new(FakeRuntime::new())));
        let job_id = start_job(state.clone()).id;
        wait_for_status(&state, job_id, JobStatus::Recording);

        Coordinator::job_pause(state.clone(), job_id).unwrap();
        assert_eq!(job_status(&state, job_id), JobStatus::Paused);
        assert_eq!(
            Coordinator::job_pause(state.clone(), job_id)
                .unwrap_err()
                .code,
            "job_not_recording"
        );

        Coordinator::job_resume(state.clone(), job_id).unwrap();
        assert_eq!(job_status(&state, job_id), JobStatus::Recording);
        Coordinator::job_stop(state.clone(), job_id).unwrap();
        wait_for_status(&state, job_id, JobStatus::Completed);
    }

    #[test]
    fn targets_list_uses_cache_while_recording() {
        let _guard = ENV_LOCK.lock().unwrap();
        isolate_env();
        let runtime = FakeRuntime::new();
        let state = Arc::new(Mutex::new(Coordinator::new(runtime.clone())));

        Coordinator::targets_list(state.clone()).unwrap();
        assert_eq!(runtime.list_calls(), 1);

        let job_id = start_job(state.clone()).id;
        wait_for_status(&state, job_id, JobStatus::Recording);
        let calls_after_start = runtime.list_calls();

        Coordinator::targets_list(state.clone()).unwrap();
        assert_eq!(runtime.list_calls(), calls_after_start);

        Coordinator::job_stop(state.clone(), job_id).unwrap();
        wait_for_status(&state, job_id, JobStatus::Completed);
    }

    fn start_job(state: SharedCoordinator<FakeRuntime>) -> crate::protocol::JobSnapshot {
        let value = Coordinator::record_start(
            state,
            StartRecordingParams {
                selector: Some(TargetSelector::Id {
                    kind: CaptureSourceKind::Display,
                    id: 1,
                }),
                options: RecordingOptions {
                    output_dir: Some(std::env::temp_dir()),
                    ..RecordingOptions::default()
                },
                duration_ms: None,
                queue: true,
            },
        )
        .unwrap();

        serde_json::from_value(value.get("job").unwrap().clone()).unwrap()
    }

    fn wait_for_status(state: &SharedCoordinator<FakeRuntime>, job_id: u64, status: JobStatus) {
        for _ in 0..50 {
            if job_status(state, job_id) == status {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "job {job_id} did not reach {status:?}; last status was {:?}",
            job_status(state, job_id)
        );
    }

    fn job_status(state: &SharedCoordinator<FakeRuntime>, job_id: u64) -> JobStatus {
        lock_state(state)
            .unwrap()
            .jobs
            .get(&job_id)
            .unwrap()
            .status
            .clone()
    }

    fn isolate_env() {
        let dir = std::env::temp_dir().join(format!(
            "wrec-daemon-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::env::set_var("WREC_HOME", dir.join("home"));
        std::env::set_var("WREC_DATA_DIR", dir.join("data"));
    }
}
