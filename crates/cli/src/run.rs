use std::{
    process::ExitCode,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use serde_json::{json, Value};
use wrec_daemon::{
    emit_error, ensure_daemon, send_request, serve_forever, wait_for_job, AgentError, DaemonClient,
    IpcResponse, JobSnapshot, JobStatus, RecordingOptions, StartRecordingParams, TargetSelector,
};

use crate::args::{DaemonCommand, JobCommand, JobsArgs, ListArgs, RecordArgs, TargetQuery};

pub fn list(args: ListArgs) -> ExitCode {
    with_daemon(args.json, || {
        let response = request_or_error("targets.list", json!({}))?;
        let targets = response
            .result
            .and_then(|value| value.get("targets").cloned())
            .unwrap_or(Value::Array(Vec::new()));

        if args.json {
            println!("{targets}");
        } else if targets.as_array().is_some_and(Vec::is_empty) {
            println!("no capture targets found");
        } else if let Some(items) = targets.as_array() {
            for item in items {
                let kind = item
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let id = item.get("id").and_then(Value::as_u64).unwrap_or_default();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                println!("{kind}\t{id}\t{name}");
            }
        }
        Ok(ExitCode::SUCCESS)
    })
}

pub fn record(args: RecordArgs) -> ExitCode {
    let json_output = args.json;
    with_daemon(json_output, || {
        let response = request_or_error(
            "record.start",
            serde_json::to_value(record_params(&args)).map_err(protocol_error)?,
        )?;
        let job = decode_job(response.result)?;
        emit_submission(&job, json_output);

        if args.detach {
            return Ok(ExitCode::SUCCESS);
        }

        if let Err(err) = install_record_interrupt_handler(job.id) {
            eprintln!("warning: Ctrl+C will not stop job {}: {err}", job.id);
        }

        let completed = wait_for_job(job.id, json_output)?;
        Ok(match completed.status {
            JobStatus::Completed => ExitCode::SUCCESS,
            JobStatus::Failed | JobStatus::Cancelled => ExitCode::FAILURE,
            _ => ExitCode::SUCCESS,
        })
    })
}

fn install_record_interrupt_handler(job_id: u64) -> Result<(), ctrlc::Error> {
    let interrupt_count = Arc::new(AtomicUsize::new(0));
    ctrlc::set_handler(
        move || match interrupt_count.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let _ = DaemonClient::new().stop_job(job_id);
            }
            _ => std::process::exit(130),
        },
    )
}

pub fn daemon(command: DaemonCommand) -> ExitCode {
    match command {
        DaemonCommand::Serve => match serve_forever() {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::FAILURE
            }
        },
        DaemonCommand::Start { json } => {
            if let Err(error) = ensure_daemon() {
                emit_error(&error, json);
                return ExitCode::FAILURE;
            }
            daemon_status(json)
        }
        DaemonCommand::Status { json } => daemon_status(json),
        DaemonCommand::Stop { json } => daemon_stop(json),
    }
}

pub fn jobs(args: JobsArgs) -> ExitCode {
    with_daemon(args.json, || {
        let response = request_or_error("jobs.list", json!({}))?;
        let result = response.result.unwrap_or_else(|| json!({ "jobs": [] }));
        if args.json {
            println!("{result}");
        } else {
            let jobs = result
                .get("jobs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if jobs.is_empty() {
                println!("no jobs known to the current daemon");
            } else {
                for job in jobs {
                    print_job_row(&job);
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    })
}

pub fn job(command: JobCommand) -> ExitCode {
    match command {
        JobCommand::Show { id, json } => job_request("job.show", id, json),
        JobCommand::Logs { id, json } => job_request("job.logs", id, json),
        JobCommand::Pause { id, json } => job_request("job.pause", id, json),
        JobCommand::Resume { id, json } => job_request("job.resume", id, json),
        JobCommand::Stop { id, json } => job_request("job.stop", id, json),
        JobCommand::Cancel { id, json } => job_request("job.cancel", id, json),
    }
}

fn daemon_status(json_output: bool) -> ExitCode {
    match send_request("daemon.status", json!({})) {
        Ok(response) if response.ok => {
            let result = response.result.unwrap_or_else(|| json!({}));
            if json_output {
                println!("{result}");
            } else {
                println!("wrec daemon running");
                if let Some(socket) = result.get("socket").and_then(Value::as_str) {
                    println!("socket: {socket}");
                }
                if let Some(home) = result.get("home").and_then(Value::as_str) {
                    println!("home: {home}");
                }
                if let Some(active) = result.get("active_job_id").and_then(Value::as_u64) {
                    println!("active job: {active}");
                }
            }
            ExitCode::SUCCESS
        }
        Ok(response) => {
            emit_error(
                &response.error.unwrap_or_else(|| AgentError {
                    code: "daemon_error".into(),
                    message: "daemon status failed without details".into(),
                    recoverable: true,
                    next: "Inspect ~/.wrec/daemon.log and retry.".into(),
                }),
                json_output,
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            emit_error(&error, json_output);
            ExitCode::FAILURE
        }
    }
}

fn daemon_stop(json_output: bool) -> ExitCode {
    match send_request("daemon.stop", json!({})) {
        Ok(response) if response.ok => {
            let result = response.result.unwrap_or_else(|| json!({}));
            if json_output {
                println!("{result}");
            } else {
                println!("wrec daemon stopping");
                if let Some(socket) = result.get("socket").and_then(Value::as_str) {
                    println!("socket: {socket}");
                }
            }
            ExitCode::SUCCESS
        }
        Ok(response) => {
            emit_error(
                &response.error.unwrap_or_else(|| AgentError {
                    code: "daemon_error".into(),
                    message: "daemon stop failed without details".into(),
                    recoverable: true,
                    next: "Inspect ~/.wrec/daemon.log and retry.".into(),
                }),
                json_output,
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            emit_error(&error, json_output);
            ExitCode::FAILURE
        }
    }
}

fn job_request(method: &str, id: u64, json_output: bool) -> ExitCode {
    with_daemon(json_output, || {
        let response = request_or_error(method, json!({ "job_id": id }))?;
        let result = response.result.unwrap_or_else(|| json!({}));
        if json_output {
            println!("{result}");
        } else if method == "job.logs" {
            for event in result
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                if let Some(message) = event.get("message").and_then(Value::as_str) {
                    println!("{message}");
                }
            }
        } else if let Some(job) = result.get("job") {
            print_job_detail(job);
        } else {
            println!("{result}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

fn with_daemon(json_output: bool, run: impl FnOnce() -> Result<ExitCode, AgentError>) -> ExitCode {
    if let Err(error) = ensure_daemon() {
        emit_error(&error, json_output);
        return ExitCode::FAILURE;
    }

    match run() {
        Ok(code) => code,
        Err(error) => {
            emit_error(&error, json_output);
            ExitCode::FAILURE
        }
    }
}

fn request_or_error(method: &str, params: Value) -> Result<IpcResponse, AgentError> {
    let response = send_request(method, params)?;
    if response.ok {
        Ok(response)
    } else {
        Err(response.error.unwrap_or_else(|| AgentError {
            code: "daemon_error".into(),
            message: format!("{method} failed without details"),
            recoverable: true,
            next: "Inspect ~/.wrec/daemon.log and retry.".into(),
        }))
    }
}

fn record_params(args: &RecordArgs) -> StartRecordingParams {
    StartRecordingParams {
        selector: target_selector(args),
        options: RecordingOptions {
            source_kind: args.source_kind,
            fps: args.fps,
            codec: args.codec,
            quality: args.quality,
            resolution: args.resolution,
            output_dir: args.output_dir.clone(),
            include_cursor: args.include_cursor,
            include_system_audio: args.include_system_audio,
            hide_wrec: args.hide_wrec,
        },
        duration_ms: args.duration.map(|duration| duration.as_millis() as u64),
        queue: args.queue,
    }
}

fn target_selector(args: &RecordArgs) -> Option<TargetSelector> {
    if let (Some(kind), Some(id)) = (args.source_kind, args.target_id) {
        return Some(TargetSelector::Id { kind, id });
    }

    match args.target_query.as_ref()? {
        TargetQuery::Name { kind, query } => Some(TargetSelector::Name {
            kind: *kind,
            query: query.clone(),
        }),
        TargetQuery::App(query) => Some(TargetSelector::App {
            query: query.clone(),
        }),
    }
}

fn decode_job(result: Option<Value>) -> Result<JobSnapshot, AgentError> {
    serde_json::from_value(
        result
            .and_then(|value| value.get("job").cloned())
            .unwrap_or(Value::Null),
    )
    .map_err(protocol_error)
}

fn protocol_error(err: serde_json::Error) -> AgentError {
    AgentError {
        code: "protocol_error".into(),
        message: err.to_string(),
        recoverable: false,
        next: "Inspect ~/.wrec/daemon.log and report this as a wrec IPC protocol bug.".into(),
    }
}

fn emit_submission(job: &JobSnapshot, json_output: bool) {
    if json_output {
        println!(
            "{}",
            json!({
                "event": "job_submitted",
                "job": job,
            })
        );
        return;
    }

    for warning in &job.warnings {
        eprintln!("warning: {}", warning.message);
        eprintln!("next: {}", warning.next);
    }
    match job.queued_position {
        Some(position) => println!("job {} queued at position {}", job.id, position),
        None => println!("job {} {}", job.id, status_label(&job.status)),
    }
}

fn print_job_row(job: &Value) {
    let id = job.get("id").and_then(Value::as_u64).unwrap_or_default();
    let status = job
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let target = job
        .get("target")
        .and_then(|target| target.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown target");
    let position = job
        .get("queued_position")
        .and_then(Value::as_u64)
        .map(|position| format!(" position={position}"))
        .unwrap_or_default();
    println!("{id}\t{status}\t{target}{position}");
}

fn print_job_detail(job: &Value) {
    print_job_row(job);
    if let Some(output) = job.get("output_path").and_then(Value::as_str) {
        println!("output: {output}");
    }
    if let Some(events) = job.get("events").and_then(Value::as_array) {
        if let Some(last) = events
            .last()
            .and_then(|event| event.get("message"))
            .and_then(Value::as_str)
        {
            println!("last event: {last}");
        }
    }
}

fn status_label(status: &JobStatus) -> &'static str {
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
