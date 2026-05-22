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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderSettings {
    pub source: CaptureSourceKind,
    pub fps: FrameRate,
    pub codec: Codec,
    pub quality: Quality,
    pub output_dir: PathBuf,
    pub include_cursor: bool,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            source: CaptureSourceKind::Display,
            fps: FrameRate::Fps30,
            codec: Codec::Hevc,
            quality: Quality::Balanced,
            output_dir: dirs_output_dir(),
            include_cursor: true,
        }
    }
}

fn dirs_output_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Movies"))
        .unwrap_or_else(|| PathBuf::from("."))
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
    fn default_settings_are_low_overhead() {
        let settings = RecorderSettings::default();

        assert_eq!(settings.source, CaptureSourceKind::Display);
        assert_eq!(settings.fps, FrameRate::Fps30);
        assert_eq!(settings.codec, Codec::Hevc);
        assert_eq!(settings.quality, Quality::Balanced);
        assert!(settings.include_cursor);
    }
}
