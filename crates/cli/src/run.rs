use std::io::BufRead;
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use wrec_core::{CaptureSourceKind, CaptureTarget, RecorderEngine, RecorderSettings};
use wrec_macos::{MacosRecorder, RecorderEvent};

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
    let settings = build_settings(&args);
    let (tx, rx) = mpsc::channel();
    let engine = Arc::new(Mutex::new(MacosRecorder::new(tx)));

    let target = match resolve_target(&engine, args.source_kind, args.target_id) {
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
    while let Ok(event) = rx.recv() {
        match event {
            RecorderEvent::Starting {
                target,
                output_path,
                ..
            } => emit(
                json,
                &format!("starting: {} -> {}", target.name, output_path.display()),
                serde_json::json!({
                    "event": "starting",
                    "target": target.name,
                    "output": output_path.display().to_string(),
                }),
            ),
            RecorderEvent::Log { message, .. } => emit(
                json,
                &message,
                serde_json::json!({ "event": "log", "message": message }),
            ),
            RecorderEvent::Metrics { metrics, .. } => emit(
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
            ),
            RecorderEvent::Failed { message, .. } => {
                emit(
                    json,
                    &format!("error: {message}"),
                    serde_json::json!({ "event": "failed", "message": message }),
                );
                code = ExitCode::FAILURE;
                break;
            }
            RecorderEvent::Exited {
                success, status, ..
            } => {
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
    id: Option<u64>,
) -> Result<CaptureTarget, String> {
    let targets = engine
        .lock()
        .unwrap()
        .list_targets()
        .map_err(|err| err.to_string())?;

    match id {
        Some(id) => targets
            .into_iter()
            .find(|target| target.id == id && target.kind == kind)
            .ok_or_else(|| format!("no {} with id {id}", kind_str(kind))),
        None => targets
            .into_iter()
            .find(|target| target.kind == kind)
            .ok_or_else(|| format!("no {} capture targets available", kind_str(kind))),
    }
}

fn build_settings(args: &RecordArgs) -> RecorderSettings {
    let defaults = RecorderSettings::default();
    RecorderSettings {
        source: args.source_kind,
        fps: args.fps,
        codec: args.codec,
        quality: args.quality,
        resolution: args.resolution,
        output_dir: args.output_dir.clone().unwrap_or(defaults.output_dir),
        include_cursor: args.include_cursor,
        include_system_audio: args.include_system_audio,
        hide_wrec: defaults.hide_wrec,
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
