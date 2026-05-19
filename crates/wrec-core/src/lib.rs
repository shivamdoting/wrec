use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSourceKind {
    Display,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    H264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Efficient,
    Balanced,
    High,
}

#[derive(Debug, Clone)]
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
            fps: FrameRate::Fps60,
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
    pub output_path: PathBuf,
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
