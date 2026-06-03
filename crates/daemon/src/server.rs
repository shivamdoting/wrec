use crate::{
    coordinator::{lock_state, Coordinator, SharedCoordinator},
    paths::{append_daemon_log, daemon_log_path, socket_path, wrec_home},
    protocol::{response_error, AgentError, IpcRequest, IpcResponse, StartRecordingParams},
    runtime::{MacosRuntime, RecordingRuntime},
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(10);
const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn serve_forever() -> Result<(), String> {
    let home = wrec_home();
    std::fs::create_dir_all(&home)
        .map_err(|err| format!("failed to create {}: {err}", home.display()))?;
    init_tracing();
    let socket = socket_path();
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(format!(
                "wrec daemon is already running at {}",
                socket.display()
            ));
        }
        std::fs::remove_file(&socket)
            .map_err(|err| format!("failed to remove stale socket {}: {err}", socket.display()))?;
    }

    append_daemon_log("daemon starting");
    let listener = UnixListener::bind(&socket)
        .map_err(|err| format!("failed to bind {}: {err}", socket.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("failed to configure {}: {err}", socket.display()))?;
    let state = Arc::new(Mutex::new(Coordinator::new(MacosRuntime)));

    while !Coordinator::shutdown_requested(&state) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let state = state.clone();
                thread::spawn(move || handle_client(stream, state));
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => append_daemon_log(format!("client accept failed: {err}")),
        }
    }

    append_daemon_log("daemon stopped");
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

fn init_tracing() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path())
    {
        Ok(file) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_ansi(false)
                .with_writer(move || file.try_clone().expect("clone daemon log file"))
                .try_init();
        }
        Err(err) => append_daemon_log(format!("tracing log open failed: {err}")),
    }
}

fn handle_client(stream: UnixStream, state: SharedCoordinator<MacosRuntime>) {
    if let Err(err) = stream.set_nonblocking(false) {
        append_daemon_log(format!("client blocking mode failed: {err}"));
    }
    let response = read_request(&stream)
        .map(|request| handle_request(request, state))
        .unwrap_or_else(|error| response_error(0, error));

    if let Err(err) = write_response(stream, &response) {
        append_daemon_log(format!("response write failed: {err}"));
    }
}

fn read_request(stream: &UnixStream) -> Result<IpcRequest, AgentError> {
    stream
        .set_read_timeout(Some(IPC_READ_TIMEOUT))
        .map_err(|err| AgentError {
            code: "request_timeout_config_failed".into(),
            message: format!("Could not configure IPC read timeout: {err}"),
            recoverable: true,
            next: "Retry the command; if it repeats, restart the daemon.".into(),
        })?;
    let reader_stream = stream.try_clone().map_err(|err| AgentError {
        code: "request_stream_clone_failed".into(),
        message: format!("Could not clone IPC stream for request read: {err}"),
        recoverable: true,
        next: "Retry the command; if it repeats, restart the daemon.".into(),
    })?;
    let mut line = String::new();
    match BufReader::new(reader_stream).read_line(&mut line) {
        Ok(0) => Err(AgentError {
            code: "empty_request".into(),
            message: "IPC request was empty".into(),
            recoverable: true,
            next: "Retry the command.".into(),
        }),
        Ok(_) => serde_json::from_str::<IpcRequest>(&line).map_err(|err| AgentError {
            code: "request_decode_failed".into(),
            message: format!("Could not decode IPC request: {err}"),
            recoverable: false,
            next: "Report this as a wrec IPC protocol bug.".into(),
        }),
        Err(err) => Err(AgentError {
            code: "request_read_failed".into(),
            message: format!("Could not read IPC request: {err}"),
            recoverable: true,
            next: "Retry the command.".into(),
        }),
    }
}

fn write_response(mut stream: UnixStream, response: &IpcResponse) -> std::io::Result<()> {
    stream.set_write_timeout(Some(IPC_WRITE_TIMEOUT))?;
    let line = serde_json::to_string(response)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

pub(crate) fn handle_request<R: RecordingRuntime>(
    request: IpcRequest,
    state: SharedCoordinator<R>,
) -> IpcResponse {
    let result = match request.method.as_str() {
        "daemon.status" => lock_state(&state).map(|state| state.status()),
        "daemon.stop" => Coordinator::daemon_stop(state),
        "targets.list" => Coordinator::targets_list(state),
        "record.start" => serde_json::from_value::<StartRecordingParams>(request.params)
            .map_err(|err| AgentError {
                code: "invalid_record_request".into(),
                message: format!("Could not parse record.start params: {err}"),
                recoverable: false,
                next: "Check the IPC request shape or use `wrec record start --help`.".into(),
            })
            .and_then(|params| Coordinator::record_start(state, params)),
        "jobs.list" => lock_state(&state).map(|state| state.jobs_list()),
        "job.show" => job_id_param(&request.params, "job.show")
            .and_then(|job_id| lock_state(&state)?.job_show(job_id)),
        "job.logs" => job_id_param(&request.params, "job.logs")
            .and_then(|job_id| lock_state(&state)?.job_logs(job_id)),
        "job.cancel" => job_id_param(&request.params, "job.cancel")
            .and_then(|job_id| Coordinator::job_cancel(state, job_id)),
        "job.pause" => job_id_param(&request.params, "job.pause")
            .and_then(|job_id| Coordinator::job_pause(state, job_id)),
        "job.resume" => job_id_param(&request.params, "job.resume")
            .and_then(|job_id| Coordinator::job_resume(state, job_id)),
        "job.stop" => job_id_param(&request.params, "job.stop")
            .and_then(|job_id| Coordinator::job_stop(state, job_id)),
        other => Err(AgentError {
            code: "unknown_method".into(),
            message: format!("Unknown IPC method `{other}`"),
            recoverable: false,
            next: "Use a supported wrec CLI command instead of calling this method directly."
                .into(),
        }),
    };

    match result {
        Ok(value) => IpcResponse {
            id: request.id,
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => response_error(request.id, error),
    }
}

fn job_id_param(params: &Value, method: &str) -> Result<u64, AgentError> {
    params
        .get("job_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| AgentError {
            code: "missing_job_id".into(),
            message: format!("{method} requires job_id"),
            recoverable: false,
            next: "Pass a numeric job id, for example `wrec job show 42`.".into(),
        })
}
