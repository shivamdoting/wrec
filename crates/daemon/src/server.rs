use crate::{
    coordinator::{lock_state, Coordinator, SharedCoordinator},
    runtime::{MacosRuntime, RecordingRuntime},
};
use control::{
    daemon_log_path, response_error, socket_path, wrec_home, AgentError, IpcRequest, IpcResponse,
    StartRecordingParams,
};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(10);
const IPC_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_signal: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// `kill <pid>` must stop the daemon like `wrec daemon stop` does. The
/// spawning shell may also hand us a mask with SIGTERM blocked (observed under
/// automation harnesses), so clear it before installing the handler.
fn install_signal_handlers() {
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
        libc::signal(
            libc::SIGTERM,
            on_terminate as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_terminate as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t,
        );
    }
}

pub fn serve_forever() -> Result<(), String> {
    let home = wrec_home();
    std::fs::create_dir_all(&home)
        .map_err(|err| format!("failed to create {}: {err}", home.display()))?;
    config::init_file_tracing(&daemon_log_path());
    // job-events.jsonl is no longer written; clean up the stale copy once.
    let _ = std::fs::remove_file(home.join("job-events.jsonl"));
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

    tracing::info!("daemon starting");
    install_signal_handlers();
    let listener = UnixListener::bind(&socket)
        .map_err(|err| format!("failed to bind {}: {err}", socket.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("failed to configure {}: {err}", socket.display()))?;
    let state = Arc::new(Mutex::new(Coordinator::new(MacosRuntime)));

    while !TERMINATED.load(Ordering::SeqCst) && !Coordinator::shutdown_requested(&state) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let state = state.clone();
                thread::spawn(move || handle_client(stream, state));
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => tracing::error!("client accept failed: {err}"),
        }
    }

    tracing::info!("daemon stopped");
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

fn handle_client(stream: UnixStream, state: SharedCoordinator<MacosRuntime>) {
    if let Err(err) = stream.set_nonblocking(false) {
        tracing::warn!("client blocking mode failed: {err}");
    }
    let response = read_request(&stream)
        .map(|request| handle_request(request, state))
        .unwrap_or_else(|error| response_error(0, error));

    if let Err(err) = write_response(stream, &response) {
        tracing::warn!("response write failed: {err}");
    }
}

fn read_request(stream: &UnixStream) -> Result<IpcRequest, AgentError> {
    if let Err(err) = stream.set_read_timeout(Some(IPC_READ_TIMEOUT)) {
        // On macOS SO_RCVTIMEO fails with EINVAL once the peer has fully
        // closed its socket. The buffered request is still readable and a
        // closed peer cannot block the read, so only other errors are fatal.
        if err.kind() != std::io::ErrorKind::InvalidInput {
            return Err(AgentError {
                code: "request_timeout_config_failed".into(),
                message: format!("Could not configure IPC read timeout: {err}"),
                recoverable: true,
                next: "Retry the command; if it repeats, restart the daemon.".into(),
            });
        }
    }
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
        "permission.status" => Coordinator::permission_status(state),
        "permission.request" => Coordinator::permission_request(state),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{env_lock, isolate_env, FakeRuntime};
    use control::{RecordingOptions, TargetSelector};
    use domain::CaptureSourceKind;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn coordinator() -> SharedCoordinator<FakeRuntime> {
        Arc::new(Mutex::new(Coordinator::new(FakeRuntime::new())))
    }

    fn request(id: u64, method: &str, params: Value) -> IpcRequest {
        IpcRequest {
            id,
            method: method.into(),
            params,
        }
    }

    fn record_start_params() -> Value {
        serde_json::to_value(StartRecordingParams {
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
        })
        .unwrap()
    }

    fn job_status(state: &SharedCoordinator<FakeRuntime>, job_id: u64) -> String {
        let response = handle_request(
            request(1, "job.show", json!({ "job_id": job_id })),
            state.clone(),
        );
        response.result.unwrap()["job"]["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn wait_for_job_status(state: &SharedCoordinator<FakeRuntime>, job_id: u64, status: &str) {
        for _ in 0..50 {
            if job_status(state, job_id) == status {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "job {job_id} did not reach {status}; last status was {}",
            job_status(state, job_id)
        );
    }

    #[test]
    fn unknown_method_error_echoes_request_id() {
        let _guard = env_lock();
        isolate_env();

        let response = handle_request(request(7, "no.such.method", json!({})), coordinator());

        assert_eq!(response.id, 7);
        assert!(!response.ok);
        assert!(response.result.is_none());
        assert_eq!(response.error.unwrap().code, "unknown_method");
    }

    #[test]
    fn job_methods_require_a_numeric_job_id() {
        let _guard = env_lock();
        isolate_env();

        let response = handle_request(
            request(1, "job.show", json!({ "job_id": "forty-two" })),
            coordinator(),
        );

        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "missing_job_id");
    }

    #[test]
    fn job_show_for_unknown_job_fails() {
        let _guard = env_lock();
        isolate_env();

        let response = handle_request(
            request(1, "job.show", json!({ "job_id": 999 })),
            coordinator(),
        );

        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "job_not_found");
    }

    #[test]
    fn record_start_rejects_malformed_params() {
        let _guard = env_lock();
        isolate_env();

        let response = handle_request(
            request(1, "record.start", json!({ "duration_ms": "long" })),
            coordinator(),
        );

        assert!(!response.ok);
        assert_eq!(response.error.unwrap().code, "invalid_record_request");
    }

    #[test]
    fn record_start_job_runs_to_completion_over_ipc() {
        let _guard = env_lock();
        isolate_env();
        let state = coordinator();

        let started = handle_request(
            request(1, "record.start", record_start_params()),
            state.clone(),
        );
        assert!(started.ok);
        let job_id = started.result.unwrap()["job"]["id"].as_u64().unwrap();
        wait_for_job_status(&state, job_id, "recording");

        let jobs = handle_request(request(2, "jobs.list", json!({})), state.clone());
        assert_eq!(jobs.result.unwrap()["active_job_id"], json!(job_id));

        let stopped = handle_request(
            request(3, "job.stop", json!({ "job_id": job_id })),
            state.clone(),
        );
        assert!(stopped.ok);
        wait_for_job_status(&state, job_id, "completed");

        let logs = handle_request(
            request(4, "job.logs", json!({ "job_id": job_id })),
            state.clone(),
        );
        assert!(!logs.result.unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn daemon_stop_refuses_while_a_job_is_active() {
        let _guard = env_lock();
        isolate_env();
        let state = coordinator();

        let started = handle_request(
            request(1, "record.start", record_start_params()),
            state.clone(),
        );
        let job_id = started.result.unwrap()["job"]["id"].as_u64().unwrap();
        wait_for_job_status(&state, job_id, "recording");

        let refused = handle_request(request(2, "daemon.stop", json!({})), state.clone());
        assert!(!refused.ok);
        assert_eq!(refused.error.unwrap().code, "daemon_busy");

        handle_request(
            request(3, "job.stop", json!({ "job_id": job_id })),
            state.clone(),
        );
        wait_for_job_status(&state, job_id, "completed");

        let stopped = handle_request(request(4, "daemon.stop", json!({})), state.clone());
        assert!(stopped.ok);
        assert_eq!(stopped.result.unwrap()["stopping"], json!(true));

        let rejected = handle_request(request(5, "record.start", record_start_params()), state);
        assert_eq!(rejected.error.unwrap().code, "daemon_stopping");
    }

    #[test]
    fn read_request_parses_a_json_line() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer
            .write_all(b"{\"id\":9,\"method\":\"daemon.status\"}\n")
            .unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();

        let request = read_request(&reader).unwrap();

        assert_eq!(request.id, 9);
        assert_eq!(request.method, "daemon.status");
        assert_eq!(request.params, Value::Null);
    }

    #[test]
    fn read_request_rejects_a_request_less_stream_as_empty() {
        let (writer, reader) = UnixStream::pair().unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(read_request(&reader).unwrap_err().code, "empty_request");
    }

    #[test]
    fn read_request_rejects_invalid_json() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.write_all(b"not json\n").unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(
            read_request(&reader).unwrap_err().code,
            "request_decode_failed"
        );
    }

    #[test]
    fn write_response_emits_a_single_json_line() {
        let (writer, reader) = UnixStream::pair().unwrap();

        write_response(
            writer,
            &IpcResponse {
                id: 4,
                ok: true,
                result: Some(json!({ "pong": true })),
                error: None,
            },
        )
        .unwrap();

        let mut line = String::new();
        BufReader::new(reader).read_line(&mut line).unwrap();
        assert!(line.ends_with('\n'));
        let parsed: IpcResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.id, 4);
        assert!(parsed.ok);
        assert_eq!(parsed.result.unwrap()["pong"], json!(true));
    }

    #[test]
    fn serve_forever_answers_requests_over_the_unix_socket() {
        let _guard = env_lock();
        let home = isolate_env();
        let server = thread::spawn(serve_forever);
        let socket = wait_for_socket();

        let status = roundtrip(&socket, &request(1, "daemon.status", json!({})));
        assert!(status.ok);
        assert_eq!(status.result.unwrap()["home"], json!(home));

        let second_daemon = serve_forever();
        assert!(second_daemon.unwrap_err().contains("already running"));

        let stopped = roundtrip(&socket, &request(2, "daemon.stop", json!({})));
        assert!(stopped.ok);

        server.join().unwrap().unwrap();
        assert!(!socket.exists());
    }

    fn wait_for_socket() -> PathBuf {
        let socket = socket_path();
        for _ in 0..250 {
            if UnixStream::connect(&socket).is_ok() {
                return socket;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "daemon socket never accepted connections at {}",
            socket.display()
        );
    }

    fn roundtrip(socket: &Path, request: &IpcRequest) -> IpcResponse {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .write_all(serde_json::to_string(request).unwrap().as_bytes())
            .unwrap();
        stream.write_all(b"\n").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}
