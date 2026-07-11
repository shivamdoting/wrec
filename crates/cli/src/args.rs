use std::path::PathBuf;
use std::time::Duration;

use domain::{CaptureSourceKind, Codec, FrameRate, Quality, Resolution};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    List(ListArgs),
    Record(RecordArgs),
    Daemon(DaemonCommand),
    Jobs(JobsArgs),
    Job(JobCommand),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListArgs {
    pub json: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct JobsArgs {
    pub json: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DaemonCommand {
    Start { json: bool },
    Status { json: bool },
    Stop { json: bool },
    Serve,
}

#[derive(Debug, PartialEq, Eq)]
pub enum JobCommand {
    Show { id: u64, json: bool },
    Logs { id: u64, json: bool },
    Pause { id: u64, json: bool },
    Resume { id: u64, json: bool },
    Stop { id: u64, json: bool },
    Cancel { id: u64, json: bool },
}

#[derive(Debug, PartialEq, Eq)]
pub struct RecordArgs {
    pub source_kind: Option<CaptureSourceKind>,
    pub target_id: Option<u64>,
    pub target_query: Option<TargetQuery>,
    pub fps: Option<FrameRate>,
    pub codec: Option<Codec>,
    pub quality: Option<Quality>,
    pub resolution: Option<Resolution>,
    pub output_dir: Option<PathBuf>,
    pub include_cursor: Option<bool>,
    pub include_system_audio: Option<bool>,
    pub include_microphone: Option<bool>,
    pub hide_wrec: Option<bool>,
    pub duration: Option<Duration>,
    pub json: bool,
    pub detach: bool,
    pub queue: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TargetQuery {
    Name {
        kind: Option<CaptureSourceKind>,
        query: String,
    },
    App(String),
}

impl Default for RecordArgs {
    fn default() -> Self {
        Self {
            source_kind: None,
            target_id: None,
            target_query: None,
            fps: None,
            codec: None,
            quality: None,
            resolution: None,
            output_dir: None,
            include_cursor: None,
            include_system_audio: None,
            include_microphone: None,
            hide_wrec: None,
            duration: None,
            json: false,
            detach: false,
            queue: true,
        }
    }
}

pub fn usage() -> String {
    "wrec - automate wrec screen recording from the terminal\n\
     \n\
     Usage:\n\
     \u{20}\u{20}wrec <command> [options]\n\
     \n\
     Commands:\n\
     \u{20}\u{20}targets [list]       List capture targets (displays and windows)\n\
     \u{20}\u{20}record start         Record with saved app settings plus per-run overrides\n\
     \u{20}\u{20}jobs                 List queued and recent recording jobs\n\
     \u{20}\u{20}job <action> <id>    Show, pause, resume, stop, cancel, or read logs\n\
     \u{20}\u{20}daemon <action>      Start/status/stop/serve the local coordinator\n\
     \u{20}\u{20}list                 Alias for targets\n\
     \u{20}\u{20}record               Alias for record start\n\
     \u{20}\u{20}help                 Show this help\n\
     \n\
     Global:\n\
     \u{20}\u{20}-h, --help           Show this help\n\
     \u{20}\u{20}-V, --version        Show the version\n\
     \n\
     list options:\n\
     \u{20}\u{20}--json               Print targets as JSON\n\
     \n\
     job options:\n\
     \u{20}\u{20}jobs --json\n\
     \u{20}\u{20}job show <id> [--json]\n\
     \u{20}\u{20}job logs <id> [--json]\n\
     \u{20}\u{20}job pause <id> [--json]\n\
     \u{20}\u{20}job resume <id> [--json]\n\
     \u{20}\u{20}job stop <id> [--json]\n\
     \u{20}\u{20}job cancel <id> [--json]\n\
     \n\
     record options:\n\
     \u{20}\u{20}--target <kind:id>    Capture a target like display:1 or window:42\n\
     \u{20}\u{20}--display <id>        Override saved source and capture a display by id\n\
     \u{20}\u{20}--window <id>         Override saved source and capture a window by id\n\
     \u{20}\u{20}--app <name>          Capture a window owned by the named app\n\
     \u{20}\u{20}--target-name <text>  Capture the display/window whose name matches text\n\
     \u{20}\u{20}--window-name <text>  Capture the window whose name matches text\n\
     \u{20}\u{20}--display-name <text> Capture the display whose name matches text\n\
     \u{20}\u{20}--fps <30|60>        Override saved frame rate\n\
     \u{20}\u{20}--codec <hevc|h264>  Override saved video codec\n\
     \u{20}\u{20}--quality <efficient|balanced|high>     Override saved quality\n\
     \u{20}\u{20}--resolution <native|720p|1080p|2k|4k>  Override saved resolution\n\
     \u{20}\u{20}--out <dir>          Override saved output directory\n\
     \u{20}\u{20}--duration <time>     Stop automatically after a duration like 30s, 5m, or 1h\n\
     \u{20}\u{20}--cursor             Capture the cursor for this recording\n\
     \u{20}\u{20}--no-cursor          Do not capture the cursor for this recording\n\
     \u{20}\u{20}--system-audio       Capture system audio for this recording\n\
     \u{20}\u{20}--no-system-audio    Do not capture system audio for this recording\n\
     \u{20}\u{20}--mic                Capture the default microphone as a separate audio track\n\
     \u{20}\u{20}--no-mic             Do not capture the microphone for this recording\n\
     \u{20}\u{20}--hide-wrec          Hide Wrec windows for this recording\n\
     \u{20}\u{20}--no-hide-wrec       Do not hide Wrec windows for this recording\n\
     \u{20}\u{20}--detach             Submit the job and return immediately\n\
     \u{20}\u{20}--queue              Queue behind the active recording (default)\n\
     \u{20}\u{20}--no-queue           Fail if another recording is active\n\
     \u{20}\u{20}--json               Emit recorder events as JSON lines\n\
     \n\
     Foreground record commands wait for the submitted job to finish. Use --detach\n\
     to return immediately, then control active work with job pause/resume/stop.\n"
        .to_string()
}

/// Parse CLI arguments. `args` must NOT include the program name (argv[0]).
pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let args = split_inline_values(args);
    let mut args = args.into_iter();

    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };

    match command.as_str() {
        "list" => parse_list(args),
        "targets" => parse_targets(args),
        "record" => parse_record_command(args),
        "daemon" => parse_daemon(args),
        "jobs" => parse_jobs(args),
        "job" => parse_job(args),
        "help" | "-h" | "--help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
}

fn parse_daemon<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let mut args = args.collect::<Vec<_>>().into_iter();
    let Some(action) = args.next() else {
        return Ok(Command::Daemon(DaemonCommand::Status { json: false }));
    };
    let json = parse_json_tail(args, "daemon")?;
    match action.as_str() {
        "start" => Ok(Command::Daemon(DaemonCommand::Start { json })),
        "status" => Ok(Command::Daemon(DaemonCommand::Status { json })),
        "stop" => Ok(Command::Daemon(DaemonCommand::Stop { json })),
        "serve" => Ok(Command::Daemon(DaemonCommand::Serve)),
        "-h" | "--help" | "help" => Ok(Command::Help),
        other => Err(format!("unknown daemon action `{other}`\n\n{}", usage())),
    }
}

fn parse_jobs<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    Ok(Command::Jobs(JobsArgs {
        json: parse_json_tail(args, "jobs")?,
    }))
}

fn parse_job<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let mut args = args.collect::<Vec<_>>().into_iter();
    let Some(action) = args.next() else {
        return Err(format!("missing job action\n\n{}", usage()));
    };
    if matches!(action.as_str(), "-h" | "--help" | "help") {
        return Ok(Command::Help);
    }
    let Some(id) = args.next() else {
        return Err(format!("missing job id for `job {action}`"));
    };
    let id = parse_u64(&id, "job id")?;
    let json = parse_json_tail(args, "job")?;
    match action.as_str() {
        "show" => Ok(Command::Job(JobCommand::Show { id, json })),
        "logs" => Ok(Command::Job(JobCommand::Logs { id, json })),
        "pause" => Ok(Command::Job(JobCommand::Pause { id, json })),
        "resume" => Ok(Command::Job(JobCommand::Resume { id, json })),
        "stop" => Ok(Command::Job(JobCommand::Stop { id, json })),
        "cancel" => Ok(Command::Job(JobCommand::Cancel { id, json })),
        other => Err(format!("unknown job action `{other}`\n\n{}", usage())),
    }
}

fn parse_targets<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let args: Vec<String> = args.collect();
    match args.first().map(String::as_str) {
        None => parse_list(args.into_iter()),
        Some("list") => parse_list(args.into_iter().skip(1)),
        Some("help" | "-h" | "--help") => Ok(Command::Help),
        Some(first) if first.starts_with('-') => parse_list(args.into_iter()),
        Some(other) => Err(format!(
            "unknown subcommand for `targets`: {other}\n\n{}",
            usage()
        )),
    }
}

fn parse_list<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let mut out = ListArgs::default();
    for arg in args {
        match arg.as_str() {
            "--json" => out.json = true,
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown option for `list`: {other}")),
        }
    }
    Ok(Command::List(out))
}

fn parse_record_command<I>(args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let args: Vec<String> = args.collect();
    match args.first().map(String::as_str) {
        None => parse_record(args.into_iter()),
        Some("start") => parse_record(args.into_iter().skip(1)),
        Some("help" | "-h" | "--help") => Ok(Command::Help),
        Some("pause" | "resume" | "stop" | "status") => Err(format!(
            "`record {}` is now handled through daemon jobs.\n\
             Use `wrec jobs --json` to find the active job, then `wrec job {} <id>`.",
            args[0],
            if args[0] == "status" {
                "show"
            } else {
                &args[0]
            }
        )),
        Some(first) if first.starts_with('-') => parse_record(args.into_iter()),
        Some(other) => Err(format!(
            "unknown subcommand for `record`: {other}\n\n{}",
            usage()
        )),
    }
}

fn parse_record<I>(mut args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let mut out = RecordArgs::default();
    let mut target_flag: Option<&'static str> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--target" => {
                set_target(&mut target_flag, "--target")?;
                let (kind, id) = parse_target(&value(&mut args, "--target")?)?;
                out.source_kind = Some(kind);
                out.target_id = Some(id);
            }
            "--display" => {
                set_target(&mut target_flag, "--display")?;
                out.source_kind = Some(CaptureSourceKind::Display);
                out.target_id = Some(parse_u64(&value(&mut args, "--display")?, "--display")?);
            }
            "--window" => {
                set_target(&mut target_flag, "--window")?;
                out.source_kind = Some(CaptureSourceKind::Window);
                out.target_id = Some(parse_u64(&value(&mut args, "--window")?, "--window")?);
            }
            "--target-name" => {
                set_target(&mut target_flag, "--target-name")?;
                out.target_query = Some(TargetQuery::Name {
                    kind: None,
                    query: value(&mut args, "--target-name")?,
                });
            }
            "--display-name" => {
                set_target(&mut target_flag, "--display-name")?;
                out.source_kind = Some(CaptureSourceKind::Display);
                out.target_query = Some(TargetQuery::Name {
                    kind: Some(CaptureSourceKind::Display),
                    query: value(&mut args, "--display-name")?,
                });
            }
            "--window-name" => {
                set_target(&mut target_flag, "--window-name")?;
                out.source_kind = Some(CaptureSourceKind::Window);
                out.target_query = Some(TargetQuery::Name {
                    kind: Some(CaptureSourceKind::Window),
                    query: value(&mut args, "--window-name")?,
                });
            }
            "--app" => {
                set_target(&mut target_flag, "--app")?;
                out.source_kind = Some(CaptureSourceKind::Window);
                out.target_query = Some(TargetQuery::App(value(&mut args, "--app")?));
            }
            "--fps" => out.fps = Some(parse_fps(&value(&mut args, "--fps")?)?),
            "--codec" => out.codec = Some(parse_codec(&value(&mut args, "--codec")?)?),
            "--quality" => out.quality = Some(parse_quality(&value(&mut args, "--quality")?)?),
            "--resolution" => {
                out.resolution = Some(parse_resolution(&value(&mut args, "--resolution")?)?)
            }
            "--out" => out.output_dir = Some(PathBuf::from(value(&mut args, "--out")?)),
            "--duration" => out.duration = Some(parse_duration(&value(&mut args, "--duration")?)?),
            "--cursor" => out.include_cursor = Some(true),
            "--no-cursor" => out.include_cursor = Some(false),
            "--system-audio" => out.include_system_audio = Some(true),
            "--no-system-audio" => out.include_system_audio = Some(false),
            "--mic" => out.include_microphone = Some(true),
            "--no-mic" => out.include_microphone = Some(false),
            "--hide-wrec" => out.hide_wrec = Some(true),
            "--no-hide-wrec" => out.hide_wrec = Some(false),
            "--detach" => out.detach = true,
            "--wait" => out.detach = false,
            "--queue" => out.queue = true,
            "--no-queue" => out.queue = false,
            "--json" => out.json = true,
            other => {
                return Err(format!(
                    "unknown option for `record`: {other}\n\n{}",
                    usage()
                ))
            }
        }
    }

    Ok(Command::Record(out))
}

fn parse_json_tail<I>(args: I, command: &str) -> Result<bool, String>
where
    I: Iterator<Item = String>,
{
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                return Err(format!("use `wrec {command} --help` from top-level help"))
            }
            other => return Err(format!("unknown option for `{command}`: {other}")),
        }
    }
    Ok(json)
}

fn set_target(current: &mut Option<&'static str>, flag: &'static str) -> Result<(), String> {
    match current {
        Some(existing) => Err(format!(
            "specify only one capture target ({existing} and {flag} both given)"
        )),
        None => {
            *current = Some(flag);
            Ok(())
        }
    }
}

fn value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{flag} expects a numeric id, got `{value}`"))
}

fn parse_target(value: &str) -> Result<(CaptureSourceKind, u64), String> {
    let Some((kind, id)) = value.split_once(':') else {
        return Err(format!(
            "invalid --target `{value}` (expected display:<id> or window:<id>)"
        ));
    };

    let kind = match kind {
        "display" => CaptureSourceKind::Display,
        "window" => CaptureSourceKind::Window,
        other => {
            return Err(format!(
                "invalid --target kind `{other}` (expected display or window)"
            ))
        }
    };

    Ok((kind, parse_u64(id, "--target")?))
}

fn parse_fps(value: &str) -> Result<FrameRate, String> {
    match value {
        "30" => Ok(FrameRate::Fps30),
        "60" => Ok(FrameRate::Fps60),
        other => Err(format!("invalid --fps `{other}` (expected 30 or 60)")),
    }
}

fn parse_codec(value: &str) -> Result<Codec, String> {
    match value {
        "hevc" => Ok(Codec::Hevc),
        "h264" => Ok(Codec::H264),
        other => Err(format!("invalid --codec `{other}` (expected hevc or h264)")),
    }
}

fn parse_quality(value: &str) -> Result<Quality, String> {
    match value {
        "efficient" => Ok(Quality::Efficient),
        "balanced" => Ok(Quality::Balanced),
        "high" => Ok(Quality::High),
        other => Err(format!(
            "invalid --quality `{other}` (expected efficient, balanced, or high)"
        )),
    }
}

fn parse_resolution(value: &str) -> Result<Resolution, String> {
    match value {
        "native" => Ok(Resolution::Native),
        "720p" => Ok(Resolution::R720p),
        "1080p" => Ok(Resolution::R1080p),
        "2k" => Ok(Resolution::R2k),
        "4k" => Ok(Resolution::R4k),
        other => Err(format!(
            "invalid --resolution `{other}` (expected native, 720p, 1080p, 2k, or 4k)"
        )),
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let Some((number, scale)) = duration_parts(value) else {
        return Err(format!(
            "invalid --duration `{value}` (expected a positive duration like 30s, 5m, or 1h)"
        ));
    };
    let amount = number
        .parse::<f64>()
        .map_err(|_| format!("invalid --duration `{value}`"))?;

    if !amount.is_finite() || amount <= 0.0 {
        return Err(format!(
            "invalid --duration `{value}` (must be greater than zero)"
        ));
    }

    Duration::try_from_secs_f64(amount * scale)
        .map_err(|_| format!("invalid --duration `{value}` (duration is too large)"))
}

fn duration_parts(value: &str) -> Option<(&str, f64)> {
    if let Some(number) = value.strip_suffix("ms") {
        return Some((number, 0.001));
    }
    if let Some(number) = value.strip_suffix('s') {
        return Some((number, 1.0));
    }
    if let Some(number) = value.strip_suffix('m') {
        return Some((number, 60.0));
    }
    if let Some(number) = value.strip_suffix('h') {
        return Some((number, 60.0 * 60.0));
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_digit())
        .then_some((value, 1.0))
}

/// Expand `--flag=value` into separate `--flag` and `value` tokens so the rest
/// of the parser only has to handle the space-separated form.
fn split_inline_values<I>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut out = Vec::new();
    for arg in args {
        if arg.starts_with("--") {
            if let Some((flag, value)) = arg.split_once('=') {
                out.push(flag.to_string());
                out.push(value.to_string());
                continue;
            }
        }
        out.push(arg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_vec(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_shows_help() {
        assert_eq!(parse_vec(&[]).unwrap(), Command::Help);
    }

    #[test]
    fn help_and_version_flags() {
        assert_eq!(parse_vec(&["help"]).unwrap(), Command::Help);
        assert_eq!(parse_vec(&["-h"]).unwrap(), Command::Help);
        assert_eq!(parse_vec(&["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_vec(&["-V"]).unwrap(), Command::Version);
        assert_eq!(parse_vec(&["--version"]).unwrap(), Command::Version);
    }

    #[test]
    fn list_defaults_and_json() {
        assert_eq!(
            parse_vec(&["list"]).unwrap(),
            Command::List(ListArgs { json: false })
        );
        assert_eq!(
            parse_vec(&["targets"]).unwrap(),
            Command::List(ListArgs { json: false })
        );
        assert_eq!(
            parse_vec(&["targets", "list", "--json"]).unwrap(),
            Command::List(ListArgs { json: true })
        );
        assert_eq!(
            parse_vec(&["list", "--json"]).unwrap(),
            Command::List(ListArgs { json: true })
        );
    }

    #[test]
    fn parses_daemon_and_job_commands() {
        assert_eq!(
            parse_vec(&["daemon", "start", "--json"]).unwrap(),
            Command::Daemon(DaemonCommand::Start { json: true })
        );
        assert_eq!(
            parse_vec(&["daemon", "stop", "--json"]).unwrap(),
            Command::Daemon(DaemonCommand::Stop { json: true })
        );
        assert_eq!(
            parse_vec(&["jobs", "--json"]).unwrap(),
            Command::Jobs(JobsArgs { json: true })
        );
        assert_eq!(
            parse_vec(&["job", "show", "42", "--json"]).unwrap(),
            Command::Job(JobCommand::Show { id: 42, json: true })
        );
        assert_eq!(
            parse_vec(&["job", "stop", "42"]).unwrap(),
            Command::Job(JobCommand::Stop {
                id: 42,
                json: false
            })
        );
        assert_eq!(
            parse_vec(&["job", "pause", "42"]).unwrap(),
            Command::Job(JobCommand::Pause {
                id: 42,
                json: false
            })
        );
        assert_eq!(
            parse_vec(&["job", "resume", "42", "--json"]).unwrap(),
            Command::Job(JobCommand::Resume { id: 42, json: true })
        );
    }

    #[test]
    fn record_uses_defaults() {
        assert_eq!(
            parse_vec(&["record"]).unwrap(),
            Command::Record(RecordArgs::default())
        );
        assert_eq!(
            parse_vec(&["record", "start"]).unwrap(),
            Command::Record(RecordArgs::default())
        );
    }

    #[test]
    fn record_parses_all_options() {
        let parsed = parse_vec(&[
            "record",
            "start",
            "--window",
            "42",
            "--fps",
            "60",
            "--codec",
            "h264",
            "--quality",
            "high",
            "--resolution",
            "4k",
            "--out",
            "/tmp/out",
            "--duration",
            "5m",
            "--no-cursor",
            "--no-system-audio",
            "--no-mic",
            "--json",
        ])
        .unwrap();

        assert_eq!(
            parsed,
            Command::Record(RecordArgs {
                source_kind: Some(CaptureSourceKind::Window),
                target_id: Some(42),
                target_query: None,
                fps: Some(FrameRate::Fps60),
                codec: Some(Codec::H264),
                quality: Some(Quality::High),
                resolution: Some(Resolution::R4k),
                output_dir: Some(PathBuf::from("/tmp/out")),
                include_cursor: Some(false),
                include_system_audio: Some(false),
                include_microphone: Some(false),
                hide_wrec: None,
                duration: Some(Duration::from_secs(5 * 60)),
                json: true,
                detach: false,
                queue: true,
            })
        );
    }

    #[test]
    fn record_accepts_inline_values() {
        let parsed = parse_vec(&["record", "start", "--fps=60", "--target=display:1"]).unwrap();
        assert_eq!(
            parsed,
            Command::Record(RecordArgs {
                source_kind: Some(CaptureSourceKind::Display),
                target_id: Some(1),
                fps: Some(FrameRate::Fps60),
                ..RecordArgs::default()
            })
        );
    }

    #[test]
    fn record_parses_queue_controls() {
        assert_eq!(
            parse_vec(&["record", "start", "--detach", "--no-queue"]).unwrap(),
            Command::Record(RecordArgs {
                detach: true,
                queue: false,
                ..RecordArgs::default()
            })
        );
    }

    #[test]
    fn record_parses_name_targets_and_duration() {
        assert_eq!(
            parse_vec(&["record", "start", "--app", "Safari", "--duration", "30s"]).unwrap(),
            Command::Record(RecordArgs {
                source_kind: Some(CaptureSourceKind::Window),
                target_query: Some(TargetQuery::App("Safari".to_string())),
                duration: Some(Duration::from_secs(30)),
                ..RecordArgs::default()
            })
        );
        assert_eq!(
            parse_vec(&["record", "start", "--window-name", "README"]).unwrap(),
            Command::Record(RecordArgs {
                source_kind: Some(CaptureSourceKind::Window),
                target_query: Some(TargetQuery::Name {
                    kind: Some(CaptureSourceKind::Window),
                    query: "README".to_string(),
                }),
                ..RecordArgs::default()
            })
        );
    }

    #[test]
    fn record_parses_positive_boolean_overrides() {
        let parsed = parse_vec(&[
            "record",
            "--cursor",
            "--system-audio",
            "--mic",
            "--hide-wrec",
        ])
        .unwrap();
        assert_eq!(
            parsed,
            Command::Record(RecordArgs {
                include_cursor: Some(true),
                include_system_audio: Some(true),
                include_microphone: Some(true),
                hide_wrec: Some(true),
                ..RecordArgs::default()
            })
        );
    }

    #[test]
    fn record_rejects_two_sources() {
        let err = parse_vec(&["record", "--display", "1", "--window", "2"]).unwrap_err();
        assert!(err.contains("only one capture target"), "{err}");
    }

    #[test]
    fn record_rejects_bad_values() {
        for args in [
            vec!["record", "--fps", "0"],
            vec!["record", "--fps", "24"],
            vec!["record", "--fps", "-30"],
            vec!["record", "--fps", "30.0"],
            vec!["record", "--fps", "invalid"],
            vec!["record", "--fps=120"],
        ] {
            let err = parse_vec(&args).unwrap_err();
            assert!(err.contains("expected 30 or 60"), "{err}");
        }
        assert!(parse_vec(&["record", "--codec", "av1"]).is_err());
        assert!(parse_vec(&["record", "--quality", "ultra"]).is_err());
        assert!(parse_vec(&["record", "--resolution", "8k"]).is_err());
        assert!(parse_vec(&["record", "--display", "abc"]).is_err());
        assert!(parse_vec(&["record", "--target", "space:1"]).is_err());
        assert!(parse_vec(&["record", "--duration", "0s"]).is_err());
        assert!(parse_vec(&["record", "--duration", "1e308h"]).is_err());
        assert!(parse_vec(&["record", "--duration", "99999999999999999999"]).is_err());
    }

    #[test]
    fn record_rejects_missing_value() {
        assert!(parse_vec(&["record", "--fps"]).is_err());
    }

    #[test]
    fn unknown_command_errors() {
        assert!(parse_vec(&["frobnicate"]).is_err());
    }
}
