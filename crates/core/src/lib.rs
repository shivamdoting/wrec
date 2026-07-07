use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSourceKind {
    #[serde(alias = "Display")]
    Display,
    #[serde(alias = "Window")]
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    #[serde(rename = "hevc", alias = "Hevc")]
    Hevc,
    #[serde(rename = "h264", alias = "H264")]
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameRate {
    #[serde(rename = "30", alias = "Fps30")]
    Fps30,
    #[serde(rename = "60", alias = "Fps60")]
    Fps60,
}

impl FrameRate {
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Fps30 => 30,
            Self::Fps60 => 60,
        }
    }

    pub const fn capped_at(self, cap: Self) -> Self {
        match (self, cap) {
            (Self::Fps60, Self::Fps30) => Self::Fps30,
            _ => self,
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
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[serde(alias = "Efficient")]
    Efficient,
    #[serde(alias = "Balanced")]
    Balanced,
    #[serde(alias = "High")]
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

    pub const fn max_resolution(self) -> Option<Resolution> {
        match self {
            Self::Efficient => Some(Resolution::R720p),
            Self::Balanced => Some(Resolution::R1080p),
            Self::High => None,
        }
    }

    pub const fn max_fps(self) -> FrameRate {
        match self {
            Self::High => FrameRate::Fps60,
            Self::Efficient | Self::Balanced => FrameRate::Fps30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Resolution {
    #[default]
    #[serde(rename = "native", alias = "Native")]
    Native,
    #[serde(rename = "720p", alias = "R720p")]
    R720p,
    #[serde(rename = "1080p", alias = "R1080p")]
    R1080p,
    #[serde(rename = "2k", alias = "R2k")]
    R2k,
    #[serde(rename = "4k", alias = "R4k")]
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

    pub const fn capped_at(self, cap: Self) -> Self {
        match cap {
            Self::Native => self,
            Self::R720p => Self::R720p,
            Self::R1080p => match self {
                Self::Native | Self::R4k | Self::R2k => Self::R1080p,
                _ => self,
            },
            Self::R2k => match self {
                Self::Native | Self::R4k => Self::R2k,
                _ => self,
            },
            Self::R4k => match self {
                Self::Native => Self::R4k,
                _ => self,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionStatus {
    Unknown,
    Granted,
    Missing,
}

impl PermissionStatus {
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
    #[serde(default = "default_resolution")]
    pub resolution: Resolution,
    pub output_dir: PathBuf,
    pub include_cursor: bool,
    #[serde(default = "default_include_system_audio")]
    pub include_system_audio: bool,
    #[serde(default)]
    pub include_microphone: bool,
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
            resolution: default_resolution(),
            output_dir: dirs_output_dir(),
            include_cursor: true,
            include_system_audio: true,
            include_microphone: false,
            hide_wrec: true,
        }
    }
}

impl RecorderSettings {
    pub fn with_preset_limits(mut self) -> Self {
        if let Some(max_resolution) = self.quality.max_resolution() {
            self.resolution = self.resolution.capped_at(max_resolution);
        }
        self.fps = self.fps.capped_at(self.quality.max_fps());
        self
    }
}

fn default_include_system_audio() -> bool {
    true
}

fn default_hide_wrec() -> bool {
    true
}

fn default_resolution() -> Resolution {
    Resolution::R1080p
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureTarget {
    pub id: u64,
    pub name: String,
    pub kind: CaptureSourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSession {
    pub id: u64,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecorderMetrics {
    pub elapsed_secs: u64,
    pub output_bytes: u64,
    pub estimated_bitrate_mbps: f32,
}

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    Starting {
        session_id: u64,
        target: CaptureTarget,
        settings: RecorderSettings,
        output_path: PathBuf,
    },
    Log {
        session_id: Option<u64>,
        message: String,
    },
    Metrics {
        session_id: u64,
        metrics: RecorderMetrics,
    },
    Failed {
        session_id: Option<u64>,
        message: String,
    },
    Exited {
        session_id: u64,
        success: bool,
        status: String,
    },
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
    fn codec_args_match_capture_engine_contract() {
        assert_eq!(Codec::Hevc.as_arg(), "hevc");
        assert_eq!(Codec::H264.as_arg(), "h264");
    }

    #[test]
    fn quality_args_match_capture_engine_contract() {
        assert_eq!(Quality::Efficient.as_arg(), "efficient");
        assert_eq!(Quality::Balanced.as_arg(), "balanced");
        assert_eq!(Quality::High.as_arg(), "high");
    }

    #[test]
    fn resolution_args_match_capture_engine_contract() {
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
        assert_eq!(settings.resolution, Resolution::R1080p);
        assert!(settings.include_cursor);
        assert!(settings.include_system_audio);
        assert!(!settings.include_microphone);
        assert!(settings.hide_wrec);
    }

    #[test]
    fn preset_limits_cap_expensive_settings() {
        assert_eq!(Quality::Efficient.max_resolution(), Some(Resolution::R720p));
        assert_eq!(Quality::Balanced.max_resolution(), Some(Resolution::R1080p));
        assert_eq!(Quality::High.max_resolution(), None);

        assert_eq!(
            FrameRate::Fps60.capped_at(FrameRate::Fps30),
            FrameRate::Fps30
        );
        assert_eq!(
            Resolution::Native.capped_at(Resolution::R1080p),
            Resolution::R1080p
        );
        assert_eq!(
            Resolution::R4k.capped_at(Resolution::R720p),
            Resolution::R720p
        );
        assert_eq!(
            Resolution::R720p.capped_at(Resolution::R1080p),
            Resolution::R720p
        );
    }

    #[test]
    fn recorder_settings_enforce_preset_limits() {
        let settings = RecorderSettings {
            quality: Quality::Efficient,
            fps: FrameRate::Fps60,
            resolution: Resolution::Native,
            ..RecorderSettings::default()
        }
        .with_preset_limits();

        assert_eq!(settings.fps, FrameRate::Fps30);
        assert_eq!(settings.resolution, Resolution::R720p);
    }
}
