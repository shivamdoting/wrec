use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use wrec_config::{store_path, AppConfig};
use wrec_core::{
    CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderMetrics, RecorderSettings,
};
use wrec_macos::{MacosRecorder, RecorderEvent};
use wrec_store::{
    now_ms, EventLevel, EventRecord, EventSource, MetricRecord, RecordingRecord, Store,
};

use crate::args::{ListArgs, RecordArgs};

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
                    "kind": kind_str(target.kind),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(items));
    } else if targets.is_empty() {
        println!("no capture targets found");
    } else {
        for target in &targets {
            println!("{}\t{}\t{}", kind_str(target.kind), target.id, target.name);
        }
    }

    ExitCode::SUCCESS
}

pub fn record(args: RecordArgs) -> ExitCode {
    let json = args.json;
    let config = AppConfig::load();
    let settings = build_settings(&config.settings, &args);
    let saved_target_id = if args.target_id.is_none() {
        selected_target_id(&config, settings.source)
    } else {
        None
    };
    let store = open_store();
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(Mutex::new(MacosRecorder::new(tx)));

    let target = match resolve_target(&engine, settings.source, args.target_id, saved_target_id) {
        Ok(target) => target,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = engine.lock().unwrap().start(target, settings) {
        eprintln!("error: {err}");
        return ExitCode::FAILURE;
    }

    install_signal_handler(engine.clone());
    spawn_stdin_controller(engine.clone());

    let mut code = ExitCode::SUCCESS;
    let mut active_output_path: Option<PathBuf> = None;
    while let Ok(event) = rx.recv() {
        match event {
            RecorderEvent::Starting {
                session_id,
                target,
                settings,
                output_path,
            } => {
                active_output_path = Some(output_path.clone());
                upsert_recording(
                    store.as_ref(),
                    session_id,
                    &target,
                    &settings,
                    output_path.clone(),
                );
                append_event(
                    store.as_ref(),
                    Some(session_id),
                    EventSource::Backend,
                    EventLevel::Info,
                    format!("starting capture: {} ({:?})", target.name, target.kind),
                );
                emit(
                    json,
                    &format!("starting: {} -> {}", target.name, output_path.display()),
                    serde_json::json!({
                        "event": "starting",
                        "target": target.name,
                        "output": output_path.display().to_string(),
                    }),
                );
            }
            RecorderEvent::Log {
                session_id,
                message,
            } => {
                append_event(
                    store.as_ref(),
                    session_id,
                    recorder_event_source(&message),
                    EventLevel::Info,
                    message.clone(),
                );
                emit(
                    json,
                    &message,
                    serde_json::json!({ "event": "log", "message": message }),
                );
            }
            RecorderEvent::Metrics {
                session_id,
                metrics,
            } => {
                append_metric(store.as_ref(), session_id, &metrics);
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
                        "elapsed_secs": metrics.elapsed_secs,
                        "output_bytes": metrics.output_bytes,
                        "bitrate_mbps": metrics.estimated_bitrate_mbps,
                    }),
                );
            }
            RecorderEvent::Failed {
                session_id,
                message,
            } => {
                if let Some(session_id) = session_id {
                    mark_recording_failed(store.as_ref(), session_id, &message);
                }
                append_event(
                    store.as_ref(),
                    session_id,
                    EventSource::Backend,
                    EventLevel::Error,
                    format!("error: {message}"),
                );
                emit(
                    json,
                    &format!("error: {message}"),
                    serde_json::json!({ "event": "failed", "message": message }),
                );
                code = ExitCode::FAILURE;
                break;
            }
            RecorderEvent::Exited {
                session_id,
                success,
                status,
                ..
            } => {
                if success {
                    mark_recording_completed(
                        store.as_ref(),
                        session_id,
                        active_output_path.as_deref(),
                    );
                } else {
                    mark_recording_failed(store.as_ref(), session_id, &status);
                }
                append_event(
                    store.as_ref(),
                    Some(session_id),
                    EventSource::Backend,
                    if success {
                        EventLevel::Info
                    } else {
                        EventLevel::Error
                    },
                    format!("helper exited: {status}"),
                );
                emit(
                    json,
                    &format!("exited: {status}"),
                    serde_json::json!({
                        "event": "exited",
                        "success": success,
                        "status": status,
                    }),
                );
                if !success {
                    code = ExitCode::FAILURE;
                }
                break;
            }
        }
    }

    code
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

fn spawn_stdin_controller(engine: Arc<Mutex<MacosRecorder>>) {
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
        // stdin reached EOF (Ctrl+D or a closed pipe): stop and finalize.
        let _ = engine.lock().unwrap().stop();
    });
}

fn resolve_target(
    engine: &Arc<Mutex<MacosRecorder>>,
    kind: CaptureSourceKind,
    explicit_id: Option<u64>,
    saved_id: Option<u64>,
) -> Result<CaptureTarget, String> {
    let targets = engine
        .lock()
        .unwrap()
        .list_targets()
        .map_err(|err| err.to_string())?;

    if let Some(id) = explicit_id {
        return targets
            .into_iter()
            .find(|target| target.id == id && target.kind == kind)
            .ok_or_else(|| format!("no {} with id {id}", kind_str(kind)));
    }

    if let Some(target) = saved_id.and_then(|id| {
        targets
            .iter()
            .find(|target| target.id == id && target.kind == kind)
            .cloned()
    }) {
        return Ok(target);
    }

    targets
        .into_iter()
        .find(|target| target.kind == kind)
        .ok_or_else(|| format!("no {} capture targets available", kind_str(kind)))
}

fn build_settings(saved: &RecorderSettings, args: &RecordArgs) -> RecorderSettings {
    let mut settings = saved.clone();

    if let Some(source) = args.source_kind {
        settings.source = source;
    }
    if let Some(fps) = args.fps {
        settings.fps = fps;
    }
    if let Some(codec) = args.codec {
        settings.codec = codec;
    }
    if let Some(quality) = args.quality {
        settings.quality = quality;
    }
    if let Some(resolution) = args.resolution {
        settings.resolution = resolution;
    }
    if let Some(output_dir) = args.output_dir.clone() {
        settings.output_dir = output_dir;
    }
    if let Some(include_cursor) = args.include_cursor {
        settings.include_cursor = include_cursor;
    }
    if let Some(include_system_audio) = args.include_system_audio {
        settings.include_system_audio = include_system_audio;
    }
    if let Some(hide_wrec) = args.hide_wrec {
        settings.hide_wrec = hide_wrec;
    }

    settings
}

fn selected_target_id(config: &AppConfig, kind: CaptureSourceKind) -> Option<u64> {
    let (selected_kind, id) = parse_target_key(config.selected_target_key.as_deref()?)?;
    (selected_kind == kind).then_some(id)
}

fn parse_target_key(key: &str) -> Option<(CaptureSourceKind, u64)> {
    let (kind, id) = key.split_once(':')?;
    let kind = match kind {
        "display" => CaptureSourceKind::Display,
        "window" => CaptureSourceKind::Window,
        _ => return None,
    };
    Some((kind, id.parse().ok()?))
}

fn open_store() -> Option<Store> {
    match Store::open(store_path()) {
        Ok(store) => Some(store),
        Err(err) => {
            eprintln!("warning: could not open recording history store: {err}");
            None
        }
    }
}

fn upsert_recording(
    store: Option<&Store>,
    session_id: u64,
    target: &CaptureTarget,
    settings: &RecorderSettings,
    output_path: PathBuf,
) {
    if let Some(store) = store {
        store.upsert_recording(RecordingRecord {
            id: session_id,
            started_at_ms: now_ms(),
            output_path,
            target_kind: kind_str(target.kind).to_string(),
            target_id: target.id,
            target_name: target.name.clone(),
            codec: settings.codec.as_arg().to_string(),
            quality: settings.quality.as_arg().to_string(),
            resolution: settings.resolution.as_arg().to_string(),
            fps: settings.fps.as_u32(),
            include_cursor: settings.include_cursor,
            include_system_audio: settings.include_system_audio,
        });
    }
}

fn mark_recording_completed(store: Option<&Store>, session_id: u64, output_path: Option<&Path>) {
    if let Some(store) = store {
        let file_size = output_path
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len());
        store.mark_recording_completed(session_id, now_ms(), file_size);
    }
}

fn mark_recording_failed(store: Option<&Store>, session_id: u64, message: &str) {
    if let Some(store) = store {
        store.mark_recording_failed(session_id, now_ms(), message.to_string());
    }
}

fn append_event(
    store: Option<&Store>,
    recording_id: Option<u64>,
    source: EventSource,
    level: EventLevel,
    message: String,
) {
    if let Some(store) = store {
        store.append_event(EventRecord {
            recording_id,
            timestamp_ms: now_ms(),
            level,
            source,
            message,
            fields_json: None,
        });
    }
}

fn append_metric(store: Option<&Store>, session_id: u64, metrics: &RecorderMetrics) {
    if let Some(store) = store {
        store.append_metric(MetricRecord {
            recording_id: session_id,
            timestamp_ms: now_ms(),
            elapsed_secs: metrics.elapsed_secs,
            output_bytes: metrics.output_bytes,
            bitrate_mbps: metrics.estimated_bitrate_mbps,
            frames: None,
            dropped_frames: None,
        });
    }
}

fn recorder_event_source(message: &str) -> EventSource {
    if message.starts_with("wrec-helper:") {
        EventSource::Helper
    } else {
        EventSource::Backend
    }
}

fn emit(json: bool, text: &str, value: serde_json::Value) {
    if json {
        println!("{value}");
    } else {
        println!("{text}");
    }
}

fn kind_str(kind: CaptureSourceKind) -> &'static str {
    match kind {
        CaptureSourceKind::Display => "display",
        CaptureSourceKind::Window => "window",
    }
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
