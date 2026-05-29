use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureSourceKind {
    Display,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    Hevc,
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameRate {
    Fps30,
    Fps60,
}

impl FrameRate {
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Fps30 => 30,
            Self::Fps60 => 60,
        }
    }
}

impl Codec {
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Hevc => "hevc",
            Self::H264 => "h264",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    Efficient,
    Balanced,
    High,
}

impl Quality {
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Efficient => "efficient",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Resolution {
    #[default]
    Native,
    R720p,
    R1080p,
    R2k,
    R4k,
}

impl Resolution {
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::R720p => "720p",
            Self::R1080p => "1080p",
            Self::R2k => "2k",
            Self::R4k => "4k",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenRecordingPermissionStatus {
    Unknown,
    Granted,
    Missing,
}

impl ScreenRecordingPermissionStatus {
    pub const fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderSettings {
    pub source: CaptureSourceKind,
    pub fps: FrameRate,
    pub codec: Codec,
    pub quality: Quality,
    #[serde(default)]
    pub resolution: Resolution,
    pub output_dir: PathBuf,
    pub include_cursor: bool,
    #[serde(default = "default_include_system_audio")]
    pub include_system_audio: bool,
    #[serde(default = "default_hide_wrec")]
    pub hide_wrec: bool,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            source: CaptureSourceKind::Display,
            fps: FrameRate::Fps30,
            codec: Codec::Hevc,
            quality: Quality::Balanced,
            resolution: Resolution::Native,
            output_dir: dirs_output_dir(),
            include_cursor: true,
            include_system_audio: true,
            hide_wrec: true,
        }
    }
}

fn default_include_system_audio() -> bool {
    true
}

fn default_hide_wrec() -> bool {
    true
}

fn dirs_output_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Movies").join(recordings_dir_name()))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(target_os = "macos")]
fn recordings_dir_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.ancestors()
                .filter_map(|path| path.file_name()?.to_str())
                .find_map(|name| name.strip_suffix(".app").map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| "Wrec".to_string())
}

#[cfg(not(target_os = "macos"))]
fn recordings_dir_name() -> String {
    "Wrec".to_string()
}

#[derive(Debug, Clone)]
pub struct CaptureTarget {
    pub id: u64,
    pub name: String,
    pub kind: CaptureSourceKind,
}

#[derive(Debug, Clone)]
pub struct RecordingSession {
    pub id: u64,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RecorderMetrics {
    pub elapsed_secs: u64,
    pub output_bytes: u64,
    pub estimated_bitrate_mbps: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum RecorderError {
    #[error("screen recording permission is not granted")]
    MissingScreenRecordingPermission,

    #[error("no capture target selected")]
    NoCaptureTarget,

    #[error("backend error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, RecorderError>;

pub trait RecorderEngine: Send {
    fn list_targets(&self) -> Result<Vec<CaptureTarget>>;
    fn start(
        &mut self,
        target: CaptureTarget,
        settings: RecorderSettings,
    ) -> Result<RecordingSession>;
    fn pause(&mut self) -> Result<()>;
    fn resume(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_args_match_helper_contract() {
        assert_eq!(Codec::Hevc.as_arg(), "hevc");
        assert_eq!(Codec::H264.as_arg(), "h264");
    }

    #[test]
    fn quality_args_match_helper_contract() {
        assert_eq!(Quality::Efficient.as_arg(), "efficient");
        assert_eq!(Quality::Balanced.as_arg(), "balanced");
        assert_eq!(Quality::High.as_arg(), "high");
    }

    #[test]
    fn resolution_args_match_helper_contract() {
        assert_eq!(Resolution::Native.as_arg(), "native");
        assert_eq!(Resolution::R720p.as_arg(), "720p");
        assert_eq!(Resolution::R1080p.as_arg(), "1080p");
        assert_eq!(Resolution::R2k.as_arg(), "2k");
        assert_eq!(Resolution::R4k.as_arg(), "4k");
    }

    #[test]
    fn default_settings_are_low_overhead() {
        let settings = RecorderSettings::default();

        assert_eq!(settings.source, CaptureSourceKind::Display);
        assert_eq!(settings.fps, FrameRate::Fps30);
        assert_eq!(settings.codec, Codec::Hevc);
        assert_eq!(settings.quality, Quality::Balanced);
        assert_eq!(settings.resolution, Resolution::Native);
        assert!(settings.include_cursor);
        assert!(settings.include_system_audio);
        assert!(settings.hide_wrec);
    }
}
