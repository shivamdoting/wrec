use std::io::BufRead;
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wrec_backend::{
    build_settings, capture_kind_arg, load_config, resolve_target, selected_target_id,
    BackendEvent, RecordingOverrides, WrecBackend,
};
use wrec_core::{
    CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderEvent, RecorderSettings,
};
use wrec_macos::MacosRecorder;

use crate::args::{ListArgs, RecordArgs, TargetQuery};

pub fn list(args: ListArgs) -> ExitCode {
    let (tx, _rx) = mpsc::channel();
    let engine = MacosRecorder::new(tx);

    let targets = match engine.list_targets() {
        Ok(targets) => targets,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    if args.json {
        let items: Vec<serde_json::Value> = targets
            .iter()
            .map(|target| {
                serde_json::json!({
                    "id": target.id,
                    "name": target.name,
                    "kind": capture_kind_arg(target.kind),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(items));
    } else if targets.is_empty() {
        println!("no capture targets found");
    } else {
        for target in &targets {
            println!(
                "{}\t{}\t{}",
                capture_kind_arg(target.kind),
                target.id,
                target.name
            );
        }
    }

    ExitCode::SUCCESS
}

pub fn record(args: RecordArgs) -> ExitCode {
    let json = args.json;
    let duration = args.duration;
    let config = load_config();
    let overrides = recording_overrides(&args);
    let mut settings = build_settings(&config.settings, &overrides);
    let saved_target_id = if overrides.target_id.is_none() && args.target_query.is_none() {
        selected_target_id(&config, settings.source)
    } else {
        None
    };
    let mut backend = WrecBackend::open();
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(Mutex::new(MacosRecorder::new(tx)));

    let targets = match engine.lock().unwrap().list_targets() {
        Ok(targets) => targets,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let target = match resolve_record_target(
        &targets,
        settings.source,
        overrides.target_id,
        saved_target_id,
        args.target_query.as_ref(),
    ) {
        Ok(target) => target,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    settings.source = target.kind;

    if let Err(err) = engine.lock().unwrap().start(target, settings) {
        while let Ok(event) = rx.try_recv() {
            if let EventAction::Exit(code) = handle_backend_event(json, &mut backend, &event) {
                return code;
            }
        }
        eprintln!("error: {err}");
        return ExitCode::FAILURE;
    }

    install_signal_handler(engine.clone());
    if let Some(duration) = duration {
        spawn_duration_controller(engine.clone(), duration);
    }
    spawn_stdin_controller(engine.clone(), duration.is_none());

    let mut code = ExitCode::SUCCESS;
    while let Ok(event) = rx.recv() {
        match handle_backend_event(json, &mut backend, &event) {
            EventAction::Continue => {}
            EventAction::MarkFailure => code = ExitCode::FAILURE,
            EventAction::Exit(exit_code) => {
                code = exit_code;
                break;
            }
        }
    }

    code
}

enum EventAction {
    Continue,
    MarkFailure,
    Exit(ExitCode),
}

fn handle_backend_event(
    json: bool,
    backend: &mut WrecBackend,
    event: &RecorderEvent,
) -> EventAction {
    match backend.handle_recorder_event(event) {
        BackendEvent::Starting {
            session_id,
            target,
            settings,
            output_path,
            ..
        } => {
            emit(
                json,
                &format!("starting: {} -> {}", target.name, output_path.display()),
                serde_json::json!({
                    "event": "starting",
                    "session_id": session_id,
                    "target": target_json(&target),
                    "settings": settings_json(&settings),
                    "output": output_path.display().to_string(),
                }),
            );
            EventAction::Continue
        }
        BackendEvent::Log {
            session_id,
            message,
            marked_started,
        } => {
            emit(
                json,
                &message,
                serde_json::json!({
                    "event": "log",
                    "session_id": session_id,
                    "message": message,
                    "marked_started": marked_started,
                }),
            );
            EventAction::Continue
        }
        BackendEvent::Metrics {
            session_id,
            metrics,
        } => {
            emit(
                json,
                &format!(
                    "{}s  {}  {:.2} Mbps",
                    metrics.elapsed_secs,
                    human_bytes(metrics.output_bytes),
                    metrics.estimated_bitrate_mbps,
                ),
                serde_json::json!({
                    "event": "metrics",
                    "session_id": session_id,
                    "elapsed_secs": metrics.elapsed_secs,
                    "output_bytes": metrics.output_bytes,
                    "bitrate_mbps": metrics.estimated_bitrate_mbps,
                }),
            );
            EventAction::Continue
        }
        BackendEvent::Failed {
            recording_id,
            message,
        } => {
            emit(
                json,
                &format!("error: {message}"),
                serde_json::json!({
                    "event": "failed",
                    "recording_id": recording_id,
                    "message": message,
                }),
            );
            EventAction::MarkFailure
        }
        BackendEvent::Exited {
            session_id,
            output_path,
            success,
            status,
        } => {
            emit(
                json,
                &format!("exited: {status}"),
                serde_json::json!({
                    "event": "exited",
                    "session_id": session_id,
                    "success": success,
                    "status": status,
                    "output": output_path.as_ref().map(|path| path.display().to_string()),
                }),
            );
            EventAction::Exit(if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn resolve_record_target(
    targets: &[CaptureTarget],
    kind: CaptureSourceKind,
    explicit_id: Option<u64>,
    saved_id: Option<u64>,
    query: Option<&TargetQuery>,
) -> Result<CaptureTarget, String> {
    match query {
        Some(query) => resolve_target_query(targets, query),
        None => resolve_target(targets, kind, explicit_id, saved_id),
    }
}

fn resolve_target_query(
    targets: &[CaptureTarget],
    query: &TargetQuery,
) -> Result<CaptureTarget, String> {
    match query {
        TargetQuery::Name { kind, query } => {
            let candidates = targets
                .iter()
                .filter(|target| kind.map_or(true, |kind| target.kind == kind))
                .collect();
            resolve_by_name(candidates, query, "target")
        }
        TargetQuery::App(query) => {
            let candidates = targets
                .iter()
                .filter(|target| target.kind == CaptureSourceKind::Window)
                .collect();
            resolve_by_app(candidates, query)
        }
    }
}

fn resolve_by_name(
    candidates: Vec<&CaptureTarget>,
    query: &str,
    label: &str,
) -> Result<CaptureTarget, String> {
    let query = normalized(query);
    if query.is_empty() {
        return Err(format!("{label} query cannot be empty"));
    }

    let exact = matches(&candidates, |target| normalized(&target.name) == query);
    if !exact.is_empty() {
        return unique_match(exact, label, &query);
    }

    let prefix = matches(&candidates, |target| {
        normalized(&target.name).starts_with(&query)
    });
    if !prefix.is_empty() {
        return unique_match(prefix, label, &query);
    }

    let contains = matches(&candidates, |target| {
        normalized(&target.name).contains(&query)
    });
    if !contains.is_empty() {
        return unique_match(contains, label, &query);
    }

    Err(format!(
        "no {label} matches `{query}`. Run `wrec-cli targets --json` and pass `--target kind:id` for an exact target."
    ))
}

fn resolve_by_app(candidates: Vec<&CaptureTarget>, query: &str) -> Result<CaptureTarget, String> {
    let query = normalized(query);
    if query.is_empty() {
        return Err("app query cannot be empty".to_string());
    }

    let exact = matches(&candidates, |target| normalized(app_name(target)) == query);
    if !exact.is_empty() {
        return unique_match(exact, "app", &query);
    }

    let prefix = matches(&candidates, |target| {
        normalized(app_name(target)).starts_with(&query)
    });
    if !prefix.is_empty() {
        return unique_match(prefix, "app", &query);
    }

    let contains = matches(&candidates, |target| {
        normalized(app_name(target)).contains(&query)
    });
    if !contains.is_empty() {
        return unique_match(contains, "app", &query);
    }

    Err(format!(
        "no app matches `{query}`. Run `wrec-cli targets --json` and pass `--target window:id` for an exact window."
    ))
}

fn matches<'a>(
    candidates: &[&'a CaptureTarget],
    predicate: impl Fn(&CaptureTarget) -> bool,
) -> Vec<&'a CaptureTarget> {
    candidates
        .iter()
        .copied()
        .filter(|target| predicate(target))
        .collect()
}

fn unique_match(
    matches: Vec<&CaptureTarget>,
    label: &str,
    query: &str,
) -> Result<CaptureTarget, String> {
    match matches.as_slice() {
        [target] => Ok((*target).clone()),
        _ => Err(format!(
            "multiple {label}s match `{query}`: {}. Pass `--target kind:id` to choose one.",
            matches
                .iter()
                .map(|target| describe_target(target))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn normalized(value: &str) -> String {
    value.trim().to_lowercase()
}

fn app_name(target: &CaptureTarget) -> &str {
    target
        .name
        .split_once(" \u{2014} ")
        .map(|(app, _)| app)
        .unwrap_or(&target.name)
}

fn describe_target(target: &CaptureTarget) -> String {
    format!(
        "{}:{} {}",
        capture_kind_arg(target.kind),
        target.id,
        target.name
    )
}

/// Stop the recording cleanly on Ctrl+C / SIGTERM / SIGHUP so the helper
/// finalizes the `.mov` instead of leaving a truncated file. After the stop
/// the helper exits, the recorder emits `Exited`, and the main loop returns.
fn install_signal_handler(engine: Arc<Mutex<MacosRecorder>>) {
    let result = ctrlc::set_handler(move || {
        eprintln!("\nstopping (signal received), finalizing recording...");
        let _ = engine.lock().unwrap().stop();
    });
    if let Err(err) = result {
        eprintln!("warning: could not install signal handler: {err}");
    }
}

fn spawn_duration_controller(engine: Arc<Mutex<MacosRecorder>>, duration: Duration) {
    thread::spawn(move || {
        thread::sleep(duration);
        eprintln!("duration elapsed, finalizing recording...");
        let _ = engine.lock().unwrap().stop();
    });
}

fn spawn_stdin_controller(engine: Arc<Mutex<MacosRecorder>>, stop_on_eof: bool) {
    thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            match line.trim().to_lowercase().as_str() {
                "" => {}
                "pause" => {
                    let _ = engine.lock().unwrap().pause();
                }
                "resume" => {
                    let _ = engine.lock().unwrap().resume();
                }
                "stop" | "q" | "quit" => {
                    let _ = engine.lock().unwrap().stop();
                    return;
                }
                other => eprintln!("unknown command `{other}` (use pause | resume | stop)"),
            }
        }
        if stop_on_eof {
            // stdin reached EOF (Ctrl+D or a closed pipe): stop and finalize.
            let _ = engine.lock().unwrap().stop();
        }
    });
}

fn recording_overrides(args: &RecordArgs) -> RecordingOverrides {
    RecordingOverrides {
        source_kind: args.source_kind,
        target_id: args.target_id,
        fps: args.fps,
        codec: args.codec,
        quality: args.quality,
        resolution: args.resolution,
        output_dir: args.output_dir.clone(),
        include_cursor: args.include_cursor,
        include_system_audio: args.include_system_audio,
        hide_wrec: args.hide_wrec,
    }
}

fn emit(json: bool, text: &str, value: serde_json::Value) {
    if json {
        let mut value = value;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "timestamp_ms".to_string(),
                serde_json::json!(timestamp_ms()),
            );
        }
        println!("{value}");
    } else {
        println!("{text}");
    }
}

fn target_json(target: &CaptureTarget) -> serde_json::Value {
    serde_json::json!({
        "kind": capture_kind_arg(target.kind),
        "id": target.id,
        "name": target.name,
    })
}

fn settings_json(settings: &RecorderSettings) -> serde_json::Value {
    serde_json::json!({
        "source": capture_kind_arg(settings.source),
        "fps": settings.fps.as_u32(),
        "codec": settings.codec.as_arg(),
        "quality": settings.quality.as_arg(),
        "resolution": settings.resolution.as_arg(),
        "output_dir": settings.output_dir.display().to_string(),
        "include_cursor": settings.include_cursor,
        "include_system_audio": settings.include_system_audio,
        "hide_wrec": settings.hide_wrec,
    })
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
