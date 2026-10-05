use crate::{backend, capture, Command};
use domain::{CaptureTarget, RecorderEvent, RecorderSettings, RecordingSession, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub const ARGUMENT: &str = "--wrec-capture-worker";
pub(crate) const PROGRAM: &str = "/proc/self/exe";
pub(crate) const EXIT_DEADLINE: Duration = Duration::from_secs(20);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_WAIT: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(100);
const MESSAGE_LIMIT: u64 = 1 << 20;
const STDERR_LINE_LIMIT: u64 = 4096;

#[derive(Serialize, Deserialize)]
pub(crate) enum Request {
    Start {
        session: RecordingSession,
        target: CaptureTarget,
        settings: RecorderSettings,
    },
    Pause,
    Resume,
    Stop,
}

#[derive(Serialize, Deserialize)]
enum Message {
    Event(RecorderEvent),
    Reply(Result<()>),
    Stopping,
    // The movie is complete; the worker is cleaning up native resources.
    Finalized,
    CleaningAttempt,
    CleanedAttempt,
}

type Received = std::result::Result<Message, String>;

pub(crate) struct Worker {
    stdin: ChildStdin,
    replies: mpsc::Receiver<Result<()>>,
    sent: u64,
    answered: u64,
    stopping: mpsc::Sender<()>,
    finished: Arc<AtomicBool>,
}

impl Worker {
    pub(crate) fn spawn(
        program: &Path,
        arguments: &[&str],
        session: RecordingSession,
        target: CaptureTarget,
        settings: RecorderSettings,
        events: mpsc::Sender<RecorderEvent>,
        deadline: Duration,
    ) -> Result<Self> {
        let mut command = std::process::Command::new(program);
        command
            .arg0("wrec-capture")
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let parent = std::process::id();
        unsafe {
            command.pre_exec(move || die_with_parent(parent));
        }
        let (ready, launched) = mpsc::sync_channel(1);
        let (replies_sender, replies) = mpsc::channel();
        let (stopping, stop_requests) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let supervisor = Supervisor {
            start: Request::Start {
                session: session.clone(),
                target,
                settings,
            },
            session,
            events,
            replies: replies_sender,
            stop_requests,
            finished: finished.clone(),
            deadline,
        };
        thread::Builder::new()
            .name("wrec-capture-supervisor".into())
            .spawn(move || supervisor.run(command, ready))
            .map_err(|error| backend(format!("could not start the capture worker: {error}")))?;
        let stdin = launched
            .recv()
            .map_err(|_| backend("could not start the capture worker"))??;
        Ok(Self {
            stdin,
            replies,
            sent: 0,
            answered: 0,
            stopping,
            finished,
        })
    }

    pub(crate) fn control(&mut self, request: Request) -> Result<()> {
        write_line(&mut self.stdin, &request).map_err(|error| {
            backend(format!(
                "the capture worker is not accepting commands: {error}"
            ))
        })?;
        self.sent += 1;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let reply = match self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(reply) => reply,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(backend("the capture worker did not reply within 5s"))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(backend("the capture worker exited before replying"))
                }
            };
            self.answered += 1;
            if self.answered == self.sent {
                return reply;
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        let _ = write_line(&mut self.stdin, &Request::Stop);
        let _ = self.stopping.send(());
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

fn die_with_parent(parent: u32) -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::getppid() } as u32 != parent {
        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
    }
    Ok(())
}

fn write_line(writer: &mut impl Write, value: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

fn set_nonblocking(stdin: &ChildStdin) -> std::io::Result<()> {
    let descriptor = stdin.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct Supervisor {
    start: Request,
    session: RecordingSession,
    events: mpsc::Sender<RecorderEvent>,
    replies: mpsc::Sender<Result<()>>,
    stop_requests: mpsc::Receiver<()>,
    finished: Arc<AtomicBool>,
    deadline: Duration,
}

impl Supervisor {
    fn run(self, mut command: std::process::Command, ready: mpsc::SyncSender<Result<ChildStdin>>) {
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = ready.send(Err(backend(format!(
                    "could not start the capture worker: {error}"
                ))));
                return;
            }
        };
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let launched = write_line(&mut stdin, &self.start)
            .and_then(|()| set_nonblocking(&stdin))
            .and_then(|()| Ok((read_messages(stdout)?, last_lines(stderr)?)));
        let (messages, last_error) = match launched {
            Ok(readers) => readers,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = ready.send(Err(backend(format!(
                    "could not start the capture worker: {error}"
                ))));
                return;
            }
        };
        if ready.send(Ok(stdin)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        let mut terminal = None;
        let mut finalized = false;
        let mut started = false;
        let mut problem = None;
        let mut output_open = true;
        let mut exit_by: Option<Instant> = None;
        let mut cleanup_by: Option<Instant> = None;
        let mut drained_by: Option<Instant> = None;
        let mut status: Option<ExitStatus> = None;
        loop {
            let received = if output_open {
                messages.recv_timeout(POLL)
            } else {
                thread::sleep(POLL);
                Err(mpsc::RecvTimeoutError::Timeout)
            };
            match received {
                Ok(Ok(Message::Event(event))) => {
                    if matches!(
                        event,
                        RecorderEvent::Exited { .. } | RecorderEvent::Cancelled { .. }
                    ) {
                        terminal = Some(event);
                        exit_by.get_or_insert(Instant::now() + self.deadline);
                    } else {
                        started |= matches!(event, RecorderEvent::Started { .. });
                        let _ = self.events.send(event);
                    }
                }
                Ok(Ok(Message::Reply(reply))) => {
                    let _ = self.replies.send(reply);
                }
                Ok(Ok(Message::Stopping)) => {
                    exit_by.get_or_insert(Instant::now() + self.deadline);
                }
                Ok(Ok(Message::Finalized)) => {
                    finalized = true;
                    exit_by.get_or_insert(Instant::now() + self.deadline);
                }
                Ok(Ok(Message::CleaningAttempt)) => {
                    cleanup_by.get_or_insert(Instant::now() + self.deadline);
                }
                Ok(Ok(Message::CleanedAttempt)) => {
                    cleanup_by = None;
                }
                Ok(Err(error)) => {
                    problem = Some(format!("capture worker sent an invalid message ({error})"));
                    let _ = child.kill();
                    status = child.wait().ok();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    output_open = false;
                    exit_by.get_or_insert(Instant::now() + self.deadline);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !matches!(
                self.stop_requests.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ) {
                exit_by.get_or_insert(Instant::now() + self.deadline);
            }
            if status.is_none() {
                status = child.try_wait().ok().flatten();
            }
            if status.is_some() {
                let drained = *drained_by.get_or_insert(Instant::now() + DRAIN_WAIT);
                if !output_open || Instant::now() >= drained {
                    break;
                }
            } else if exit_by.is_some_and(|by| Instant::now() >= by) {
                let _ = child.kill();
                status = child.wait().ok();
                if terminal.is_none() {
                    problem = Some(format!(
                        "capture worker did not exit within {}s",
                        self.deadline.as_secs()
                    ));
                }
                break;
            } else if cleanup_by.is_some_and(|by| Instant::now() >= by) {
                let _ = child.kill();
                status = child.wait().ok();
                if terminal.is_none() {
                    problem = Some(format!(
                        "capture worker did not finish stopping an encoder attempt within {}s",
                        self.deadline.as_secs()
                    ));
                }
                break;
            }
        }
        drop(self.replies);
        // The movie was complete before native cleanup hung or crashed.
        if terminal.is_none() && finalized {
            let problem = problem
                .take()
                .unwrap_or_else(|| "capture worker exited during cleanup".to_string());
            terminal = Some(RecorderEvent::Exited {
                session_id: self.session.id,
                success: true,
                status: format!(
                    "recording finalized, but the {}",
                    problem.trim_start_matches("the ")
                ),
            });
        }
        let event = match (terminal, problem) {
            (Some(event), None) => event,
            (_, problem) => {
                let empty =
                    std::fs::metadata(&self.session.output_path).is_ok_and(|file| file.len() == 0);
                if !started || empty {
                    let _ = std::fs::remove_file(&self.session.output_path);
                }
                let problem =
                    problem.unwrap_or_else(|| "capture worker exited unexpectedly".to_string());
                let status = status
                    .map_or_else(|| "unknown status".to_string(), |status| status.to_string());
                let detail = last_error.recv_timeout(DRAIN_WAIT).unwrap_or_default();
                RecorderEvent::Exited {
                    session_id: self.session.id,
                    success: false,
                    status: if detail.is_empty() {
                        format!("{problem} ({status})")
                    } else {
                        format!("{problem} ({status}): {detail}")
                    },
                }
            }
        };
        self.finished.store(true, Ordering::SeqCst);
        let _ = self.events.send(event);
    }
}

fn read_messages(stdout: ChildStdout) -> std::io::Result<mpsc::Receiver<Received>> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("wrec-capture-output".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader
                    .by_ref()
                    .take(MESSAGE_LIMIT)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        return;
                    }
                }
                if !line.ends_with(b"\n") && (line.len() as u64) < MESSAGE_LIMIT {
                    return;
                }
                let message = serde_json::from_slice(&line).map_err(|error| error.to_string());
                let invalid = message.is_err();
                if sender.send(message).is_err() || invalid {
                    return;
                }
            }
        })?;
    Ok(receiver)
}

fn last_lines(stderr: ChildStderr) -> std::io::Result<mpsc::Receiver<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("wrec-capture-stderr".into())
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            let mut previous = String::new();
            let mut last = String::new();
            loop {
                line.clear();
                match reader
                    .by_ref()
                    .take(STDERR_LINE_LIMIT)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&line).trim().to_string();
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    String::from_utf8_lossy(&line).trim_end()
                );
                if !text.is_empty() {
                    previous = std::mem::replace(&mut last, text);
                }
            }
            let _ = sender.send(format!("{previous} {last}").trim().to_string());
        })?;
    Ok(receiver)
}

pub fn run() -> ! {
    unsafe {
        libc::prctl(libc::PR_SET_NAME, c"wrec-capture".as_ptr() as libc::c_ulong);
    }
    let protocol = match std::io::stdout().as_fd().try_clone_to_owned() {
        Ok(protocol) => File::from(protocol),
        Err(error) => fail(format!("could not open the protocol stream: {error}")),
    };
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        fail(format!(
            "could not redirect stdout: {}",
            std::io::Error::last_os_error()
        ));
    }
    let output = Arc::new(Mutex::new(protocol));
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let Ok(Request::Start {
        session,
        target,
        settings,
    }) = serde_json::from_str(&line)
    else {
        fail(format!("expected a start request, received {line:?}"));
    };
    let (events, outgoing) = mpsc::channel();
    let (commands, receiver) = mpsc::sync_channel(1);
    let (stop, _) = watch::channel(false);
    let (pending, answers) = mpsc::channel::<mpsc::Receiver<Result<()>>>();
    let reply_output = output.clone();
    let control_stop = stop.clone();
    spawn("wrec-capture-replies", move || {
        for answer in answers {
            let reply = answer
                .recv()
                .unwrap_or_else(|_| Err(backend("recording stopped before replying")));
            send(&reply_output, &Message::Reply(reply));
        }
    });
    spawn("wrec-capture-control", move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str(&line) {
                Ok(Request::Pause) => queue(&commands, &pending, Command::Pause),
                Ok(Request::Resume) => queue(&commands, &pending, Command::Resume),
                Ok(Request::Stop) => {
                    control_stop.send_replace(true);
                }
                _ => report(&format!("ignored an invalid request: {line:?}")),
            }
        }
        control_stop.send_replace(true);
    });
    let stopping_output = output.clone();
    let stopping = Arc::new(move |teardown: crate::pipeline::Teardown| {
        send(
            &stopping_output,
            &match teardown {
                crate::pipeline::Teardown::Recording => Message::Stopping,
                crate::pipeline::Teardown::Finalized => Message::Finalized,
                crate::pipeline::Teardown::AttemptStarted => Message::CleaningAttempt,
                crate::pipeline::Teardown::AttemptFinished => Message::CleanedAttempt,
            },
        )
    });
    let forwarder = spawn("wrec-capture-events", move || {
        for event in outgoing {
            let finished = matches!(
                event,
                RecorderEvent::Exited { .. } | RecorderEvent::Cancelled { .. }
            );
            send(&output, &Message::Event(event));
            if finished {
                break;
            }
        }
    });
    capture(target, settings, session, events, receiver, stop, stopping);
    let _ = forwarder.join();
    std::process::exit(0)
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .unwrap_or_else(|error| fail(format!("could not start {name}: {error}")))
}

fn queue(
    commands: &mpsc::SyncSender<Command>,
    pending: &mpsc::Sender<mpsc::Receiver<Result<()>>>,
    command: fn(mpsc::SyncSender<Result<()>>) -> Command,
) {
    let (reply, answer) = mpsc::sync_channel(1);
    if let Err(error) = commands.try_send(command(reply.clone())) {
        let _ = reply.send(Err(backend(error)));
    }
    let _ = pending.send(answer);
}

fn send(output: &Mutex<File>, message: &Message) {
    let mut output = output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = write_line(&mut *output, message);
}

fn report(message: &str) {
    let _ = writeln!(std::io::stderr(), "capture worker: {message}");
}

fn fail(message: String) -> ! {
    report(&message);
    std::process::exit(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{CaptureSourceKind, RecorderError};
    use std::sync::atomic::AtomicU64;

    const TEST_DEADLINE: Duration = Duration::from_secs(1);

    struct Fake {
        worker: Option<Worker>,
        events: mpsc::Receiver<RecorderEvent>,
        session: RecordingSession,
    }

    impl Fake {
        fn start(script: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let session = RecordingSession {
                id,
                output_path: std::env::temp_dir()
                    .join(format!("wrec-worker-test-{}-{id}.mov", std::process::id())),
            };
            let (events, received) = mpsc::channel();
            let worker = Worker::spawn(
                Path::new("/bin/sh"),
                &[
                    "-c",
                    &format!(r#"echo $$ > "$0.pid"; {script}"#),
                    session.output_path.to_str().unwrap(),
                ],
                session.clone(),
                CaptureTarget {
                    id: 0,
                    name: "display".into(),
                    kind: CaptureSourceKind::Display,
                },
                RecorderSettings::default(),
                events,
                TEST_DEADLINE,
            )
            .unwrap();
            Self {
                worker: Some(worker),
                events: received,
                session,
            }
        }

        fn worker(&mut self) -> &mut Worker {
            self.worker.as_mut().unwrap()
        }

        fn terminal(&self) -> RecorderEvent {
            loop {
                let event = self.events.recv_timeout(Duration::from_secs(10)).unwrap();
                if matches!(
                    event,
                    RecorderEvent::Exited { .. } | RecorderEvent::Cancelled { .. }
                ) {
                    if let Some(worker) = &self.worker {
                        assert!(worker.finished(), "terminal event published before reaping");
                    }
                    assert!(self.reaped(), "terminal event published before reaping");
                    return event;
                }
            }
        }

        fn failure(&self) -> String {
            match self.terminal() {
                RecorderEvent::Exited {
                    success: false,
                    status,
                    ..
                } => status,
                event => panic!("expected a failed exit, received {event:?}"),
            }
        }

        fn reaped(&self) -> bool {
            let pid = std::fs::read_to_string(self.pid_file()).unwrap();
            !Path::new(&format!("/proc/{}", pid.trim())).exists()
        }

        fn pid_file(&self) -> std::path::PathBuf {
            self.session.output_path.with_extension("mov.pid")
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.pid_file());
            let _ = std::fs::remove_file(&self.session.output_path);
        }
    }

    #[test]
    fn protocol_round_trips_replies_and_events() {
        let line = serde_json::to_string(&Message::Reply(Err(RecorderError::Backend(
            "not ready".into(),
        ))))
        .unwrap();
        let Message::Reply(Err(RecorderError::Backend(message))) =
            serde_json::from_str(&line).unwrap()
        else {
            panic!("reply changed shape: {line}")
        };
        assert_eq!(message, "not ready");
        let line = serde_json::to_string(&Message::Event(RecorderEvent::Exited {
            session_id: 7,
            success: true,
            status: "recording finalized".into(),
        }))
        .unwrap();
        assert!(matches!(
            serde_json::from_str(&line).unwrap(),
            Message::Event(RecorderEvent::Exited {
                session_id: 7,
                success: true,
                ..
            })
        ));
    }

    #[test]
    fn worker_crash_fails_the_recording_with_its_last_error() {
        let fake = Fake::start(
            r#"read start; : > "$0"; echo 'XIO: fatal IO error on X server' >&2; echo '      after 9 requests' >&2; exit 1"#,
        );
        let status = fake.failure();
        assert!(status.contains("exited unexpectedly"), "{status}");
        assert!(status.contains("exit status: 1"), "{status}");
        assert!(
            status.ends_with("XIO: fatal IO error on X server after 9 requests"),
            "{status}"
        );
        assert!(!fake.session.output_path.exists());
    }

    #[test]
    fn worker_signal_after_frames_keeps_the_partial_movie() {
        let fake = Fake::start(
            r#"read start; echo partial > "$0"; echo '{"Event":{"Started":{"session_id":0,"dimensions":null}}}'; kill -9 $$"#,
        );
        let status = fake.failure();
        assert!(status.contains("SIGKILL"), "{status}");
        assert!(fake.session.output_path.exists());
    }

    #[test]
    fn worker_crash_before_a_fragment_is_written_removes_the_empty_movie() {
        let fake = Fake::start(
            r#"read start; : > "$0"; echo '{"Event":{"Started":{"session_id":0,"dimensions":null}}}'; exit 1"#,
        );
        fake.failure();
        assert!(!fake.session.output_path.exists());
    }

    #[test]
    fn replies_reach_the_waiting_command_and_finish_follows_reaping() {
        let mut fake = Fake::start(
            r#"read start; read pause; echo '{"Reply":{"Err":{"Backend":"not ready"}}}'; read resume; echo '{"Reply":{"Ok":null}}'; read stop; echo '{"Event":{"Cancelled":{"session_id":0}}}'"#,
        );
        assert!(
            matches!(fake.worker().control(Request::Pause), Err(RecorderError::Backend(message)) if message == "not ready")
        );
        fake.worker().control(Request::Resume).unwrap();
        fake.worker().stop();
        assert!(matches!(fake.terminal(), RecorderEvent::Cancelled { .. }));
        assert!(matches!(
            fake.events.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn late_replies_are_not_given_to_the_next_command() {
        let mut fake = Fake::start(
            r#"read start; read pause; sleep 6; echo '{"Reply":{"Err":{"Backend":"late pause"}}}'; read resume; echo '{"Reply":{"Ok":null}}'; read stop; exit 0"#,
        );
        let error = fake.worker().control(Request::Pause).unwrap_err();
        assert!(
            error.to_string().contains("did not reply within 5s"),
            "{error}"
        );
        fake.worker().control(Request::Resume).unwrap();
    }

    #[test]
    fn worker_death_while_a_command_waits_returns_promptly() {
        let mut fake = Fake::start("read start; read pause; exit 3");
        let at = Instant::now();
        let error = fake.worker().control(Request::Pause).unwrap_err();
        assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
        assert!(
            error.to_string().contains("exited before replying"),
            "{error}"
        );
        assert!(fake.failure().contains("exit status: 3"));
        assert!(fake.worker().control(Request::Resume).is_err());
    }

    #[test]
    fn invalid_worker_output_stops_the_worker() {
        let fake = Fake::start("read start; echo not-json; exec sleep 30");
        let at = Instant::now();
        let status = fake.failure();
        assert!(status.contains("invalid message"), "{status}");
        assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
    }

    #[test]
    fn frozen_worker_is_killed_after_the_stop_deadline() {
        let mut fake = Fake::start(
            r#"read start; echo '{"Event":{"Started":{"session_id":0,"dimensions":null}}}'; kill -STOP $$"#,
        );
        assert!(matches!(
            fake.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            RecorderEvent::Started { .. }
        ));
        let error = fake.worker().control(Request::Pause).unwrap_err();
        assert!(
            error.to_string().contains("did not reply within 5s"),
            "{error}"
        );
        assert!(!fake.worker().finished());
        let at = Instant::now();
        fake.worker().stop();
        let status = fake.failure();
        assert!(at.elapsed() >= TEST_DEADLINE, "{:?}", at.elapsed());
        assert!(status.contains("did not exit within 1s"), "{status}");
        assert!(status.contains("SIGKILL"), "{status}");
    }

    #[test]
    fn final_event_waits_until_a_hung_worker_is_killed_and_reaped() {
        let mut fake = Fake::start(
            r#"read start; echo '{"Event":{"Exited":{"session_id":0,"success":true,"status":"recording finalized"}}}'; exec sleep 30"#,
        );
        assert!(matches!(
            fake.events.recv_timeout(TEST_DEADLINE / 2),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!fake.worker().finished());
        assert!(matches!(
            fake.terminal(),
            RecorderEvent::Exited { success: true, .. }
        ));
    }

    #[test]
    fn teardown_that_hangs_after_a_failure_is_killed_without_a_stop_request() {
        let fake = Fake::start(
            r#"read start; echo '{"Event":{"Started":{"session_id":0,"dimensions":null}}}'; echo '"Stopping"'; exec sleep 30"#,
        );
        let at = Instant::now();
        let status = fake.failure();
        assert!(at.elapsed() >= TEST_DEADLINE, "{:?}", at.elapsed());
        assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
        assert!(status.contains("did not exit within 1s"), "{status}");
    }

    #[test]
    fn a_hung_teardown_between_encoder_attempts_is_killed_without_a_stop_request() {
        let fake = Fake::start(r#"read start; echo '"CleaningAttempt"'; exec sleep 30"#);
        let at = Instant::now();
        let status = fake.failure();
        assert!(at.elapsed() >= TEST_DEADLINE, "{:?}", at.elapsed());
        assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
        assert!(
            status.contains("did not finish stopping an encoder attempt within 1s"),
            "{status}"
        );
        assert!(status.contains("SIGKILL"), "{status}");
    }

    #[test]
    fn repeated_attempt_teardown_reports_keep_the_first_deadline() {
        let fake = Fake::start(
            r#"read start; for report in 1 2 3 4 5 6; do echo '"CleaningAttempt"'; sleep 0.4; done; exec sleep 30"#,
        );
        let at = Instant::now();
        let status = fake.failure();
        assert!(at.elapsed() >= TEST_DEADLINE, "{:?}", at.elapsed());
        assert!(
            at.elapsed() < Duration::from_millis(1800),
            "{:?}",
            at.elapsed()
        );
        assert!(
            status.contains("did not finish stopping an encoder attempt within 1s"),
            "{status}"
        );
    }

    #[test]
    fn a_finished_attempt_teardown_does_not_limit_the_next_attempt() {
        let fake = Fake::start(
            r#"read start; echo '"CleaningAttempt"'; echo '"CleanedAttempt"'; echo '{"Event":{"Started":{"session_id":0,"dimensions":null}}}'; sleep 3; echo '{"Event":{"Exited":{"session_id":0,"success":true,"status":"recording finalized"}}}'"#,
        );
        let at = Instant::now();
        assert!(matches!(
            fake.terminal(),
            RecorderEvent::Exited { success: true, .. }
        ));
        assert!(at.elapsed() >= 2 * TEST_DEADLINE, "{:?}", at.elapsed());
    }

    #[test]
    fn stopping_during_attempt_teardown_keeps_the_stop_deadline() {
        let mut fake = Fake::start(
            r#"read start; echo '"CleaningAttempt"'; read stop; sleep 0.5; echo '"CleanedAttempt"'; exec sleep 30"#,
        );
        thread::sleep(Duration::from_millis(300));
        let at = Instant::now();
        fake.worker().stop();
        let status = fake.failure();
        assert!(at.elapsed() >= TEST_DEADLINE, "{:?}", at.elapsed());
        assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
        assert!(status.contains("did not exit within 1s"), "{status}");
    }

    #[test]
    fn result_reported_before_a_hung_teardown_is_kept() {
        let fake = Fake::start(
            r#"read start; echo '"Stopping"'; echo '{"Event":{"Exited":{"session_id":0,"success":false,"status":"source lost"}}}'; exec sleep 30"#,
        );
        assert!(matches!(
            fake.terminal(),
            RecorderEvent::Exited { success: false, status, .. } if status == "source lost"
        ));
    }

    #[test]
    fn a_finalized_movie_survives_a_hung_cleanup() {
        let fake = Fake::start(r#"read start; echo '"Finalized"'; exec sleep 30"#);
        let terminal = fake.terminal();
        assert!(
            matches!(
                &terminal,
                RecorderEvent::Exited { success: true, status, .. }
                    if status.starts_with("recording finalized, but the capture worker did not exit within 1s")
            ),
            "{terminal:?}"
        );
    }

    #[test]
    fn exiting_worker_is_finished_when_its_final_event_arrives() {
        let fake = Fake::start(
            r#"read start; echo '{"Event":{"Exited":{"session_id":0,"success":true,"status":"recording finalized"}}}'"#,
        );
        let at = Instant::now();
        assert!(matches!(
            fake.terminal(),
            RecorderEvent::Exited { success: true, .. }
        ));
        assert!(at.elapsed() < TEST_DEADLINE, "{:?}", at.elapsed());
    }

    #[test]
    fn dropping_the_handle_of_a_hung_worker_kills_it_after_the_deadline() {
        let mut fake = Fake::start("read start; exec sleep 30");
        let at = Instant::now();
        drop(fake.worker.take());
        assert!(at.elapsed() < Duration::from_millis(200));
        assert!(fake.failure().contains("did not exit within 1s"));
    }

    #[test]
    fn dropping_the_handle_lets_a_responsive_worker_stop() {
        let mut fake = Fake::start("read start; read stop; exit 4");
        drop(fake.worker.take());
        assert!(fake.failure().contains("exit status: 4"));
    }
}
