use std::path::PathBuf;

use wrec_core::{CaptureSourceKind, Codec, FrameRate, Quality, Resolution};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    List(ListArgs),
    Record(RecordArgs),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListArgs {
    pub json: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RecordArgs {
    pub source_kind: Option<CaptureSourceKind>,
    pub target_id: Option<u64>,
    pub fps: Option<FrameRate>,
    pub codec: Option<Codec>,
    pub quality: Option<Quality>,
    pub resolution: Option<Resolution>,
    pub output_dir: Option<PathBuf>,
    pub include_cursor: Option<bool>,
    pub include_system_audio: Option<bool>,
    pub hide_wrec: Option<bool>,
    pub json: bool,
}

impl Default for RecordArgs {
    fn default() -> Self {
        Self {
            source_kind: None,
            target_id: None,
            fps: None,
            codec: None,
            quality: None,
            resolution: None,
            output_dir: None,
            include_cursor: None,
            include_system_audio: None,
            hide_wrec: None,
            json: false,
        }
    }
}

pub fn usage() -> String {
    "wrec-cli - automate wrec screen recording from the terminal\n\
     \n\
     Usage:\n\
     \u{20}\u{20}wrec-cli <command> [options]\n\
     \n\
     Commands:\n\
     \u{20}\u{20}list                 List capture targets (displays and windows)\n\
     \u{20}\u{20}record               Record with saved app settings (foreground; control via stdin)\n\
     \u{20}\u{20}help                 Show this help\n\
     \n\
     Global:\n\
     \u{20}\u{20}-h, --help           Show this help\n\
     \u{20}\u{20}-V, --version        Show the version\n\
     \n\
     list options:\n\
     \u{20}\u{20}--json               Print targets as JSON\n\
     \n\
     record options:\n\
     \u{20}\u{20}--display <id>        Override saved source and capture a display by id\n\
     \u{20}\u{20}--window <id>         Override saved source and capture a window by id\n\
     \u{20}\u{20}--fps <30|60>        Override saved frame rate\n\
     \u{20}\u{20}--codec <hevc|h264>  Override saved video codec\n\
     \u{20}\u{20}--quality <efficient|balanced|high>     Override saved quality\n\
     \u{20}\u{20}--resolution <native|720p|1080p|2k|4k>  Override saved resolution\n\
     \u{20}\u{20}--out <dir>          Override saved output directory\n\
     \u{20}\u{20}--cursor             Capture the cursor for this recording\n\
     \u{20}\u{20}--no-cursor          Do not capture the cursor for this recording\n\
     \u{20}\u{20}--system-audio       Capture system audio for this recording\n\
     \u{20}\u{20}--no-system-audio    Do not capture system audio for this recording\n\
     \u{20}\u{20}--hide-wrec          Hide Wrec windows for this recording\n\
     \u{20}\u{20}--no-hide-wrec       Do not hide Wrec windows for this recording\n\
     \u{20}\u{20}--json               Emit recorder events as JSON lines\n\
     \n\
     While recording, type a command on stdin and press Enter:\n\
     \u{20}\u{20}pause   resume   stop\n\
     Closing stdin (Ctrl+D / a closed pipe) also stops and finalizes the file.\n"
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
        "record" => parse_record(args),
        "help" | "-h" | "--help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
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

fn parse_record<I>(mut args: I) -> Result<Command, String>
where
    I: Iterator<Item = String>,
{
    let mut out = RecordArgs::default();
    let mut source_flag: Option<&'static str> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--display" => {
                set_source(&mut source_flag, "--display")?;
                out.source_kind = Some(CaptureSourceKind::Display);
                out.target_id = Some(parse_u64(&value(&mut args, "--display")?, "--display")?);
            }
            "--window" => {
                set_source(&mut source_flag, "--window")?;
                out.source_kind = Some(CaptureSourceKind::Window);
                out.target_id = Some(parse_u64(&value(&mut args, "--window")?, "--window")?);
            }
            "--fps" => out.fps = Some(parse_fps(&value(&mut args, "--fps")?)?),
            "--codec" => out.codec = Some(parse_codec(&value(&mut args, "--codec")?)?),
            "--quality" => out.quality = Some(parse_quality(&value(&mut args, "--quality")?)?),
            "--resolution" => {
                out.resolution = Some(parse_resolution(&value(&mut args, "--resolution")?)?)
            }
            "--out" => out.output_dir = Some(PathBuf::from(value(&mut args, "--out")?)),
            "--cursor" => out.include_cursor = Some(true),
            "--no-cursor" => out.include_cursor = Some(false),
            "--system-audio" => out.include_system_audio = Some(true),
            "--no-system-audio" => out.include_system_audio = Some(false),
            "--hide-wrec" => out.hide_wrec = Some(true),
            "--no-hide-wrec" => out.hide_wrec = Some(false),
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

fn set_source(current: &mut Option<&'static str>, flag: &'static str) -> Result<(), String> {
    match current {
        Some(existing) => Err(format!(
            "specify only one capture source ({existing} and {flag} both given)"
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
            parse_vec(&["list", "--json"]).unwrap(),
            Command::List(ListArgs { json: true })
        );
    }

    #[test]
    fn record_uses_defaults() {
        assert_eq!(
            parse_vec(&["record"]).unwrap(),
            Command::Record(RecordArgs::default())
        );
    }

    #[test]
    fn record_parses_all_options() {
        let parsed = parse_vec(&[
            "record",
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
            "--no-cursor",
            "--no-system-audio",
            "--json",
        ])
        .unwrap();

        assert_eq!(
            parsed,
            Command::Record(RecordArgs {
                source_kind: Some(CaptureSourceKind::Window),
                target_id: Some(42),
                fps: Some(FrameRate::Fps60),
                codec: Some(Codec::H264),
                quality: Some(Quality::High),
                resolution: Some(Resolution::R4k),
                output_dir: Some(PathBuf::from("/tmp/out")),
                include_cursor: Some(false),
                include_system_audio: Some(false),
                hide_wrec: None,
                json: true,
            })
        );
    }

    #[test]
    fn record_accepts_inline_values() {
        let parsed = parse_vec(&["record", "--fps=60", "--display=1"]).unwrap();
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
    fn record_parses_positive_boolean_overrides() {
        let parsed = parse_vec(&["record", "--cursor", "--system-audio", "--hide-wrec"]).unwrap();
        assert_eq!(
            parsed,
            Command::Record(RecordArgs {
                include_cursor: Some(true),
                include_system_audio: Some(true),
                hide_wrec: Some(true),
                ..RecordArgs::default()
            })
        );
    }

    #[test]
    fn record_rejects_two_sources() {
        let err = parse_vec(&["record", "--display", "1", "--window", "2"]).unwrap_err();
        assert!(err.contains("only one capture source"), "{err}");
    }

    #[test]
    fn record_rejects_bad_values() {
        assert!(parse_vec(&["record", "--fps", "24"]).is_err());
        assert!(parse_vec(&["record", "--codec", "av1"]).is_err());
        assert!(parse_vec(&["record", "--quality", "ultra"]).is_err());
        assert!(parse_vec(&["record", "--resolution", "8k"]).is_err());
        assert!(parse_vec(&["record", "--display", "abc"]).is_err());
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
