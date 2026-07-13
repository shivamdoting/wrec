use domain::{
    CaptureSourceKind, CaptureTarget, Codec, FrameRate, Quality, RecorderMetrics, RecorderSettings,
    Resolution,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AgentError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
    pub next: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWarning {
    pub code: String,
    pub message: String,
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Starting,
    Recording,
    Paused,
    Finishing,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TargetSelector {
    Id {
        kind: CaptureSourceKind,
        id: u64,
    },
    Name {
        kind: Option<CaptureSourceKind>,
        query: String,
    },
    App {
        query: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordingOptions {
    pub source_kind: Option<CaptureSourceKind>,
    pub fps: Option<FrameRate>,
    pub codec: Option<Codec>,
    pub quality: Option<Quality>,
    pub resolution: Option<Resolution>,
    pub output_dir: Option<PathBuf>,
    pub include_cursor: Option<bool>,
    pub include_system_audio: Option<bool>,
    pub include_microphone: Option<bool>,
    pub hide_wrec: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRecordingParams {
    pub selector: Option<TargetSelector>,
    #[serde(default)]
    pub options: RecordingOptions,
    pub duration_ms: Option<u64>,
    #[serde(default = "default_queue")]
    pub queue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub id: u64,
    pub status: JobStatus,
    pub selector: Option<TargetSelector>,
    pub target: Option<CaptureTarget>,
    pub settings: Option<RecorderSettings>,
    pub output_path: Option<PathBuf>,
    pub queued_position: Option<usize>,
    pub warnings: Vec<AgentWarning>,
    pub events: Vec<JobEvent>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEvent {
    pub timestamp_ms: u64,
    pub level: EventLevel,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<RecorderMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

fn default_queue() -> bool {
    true
}

pub fn response_error(id: u64, error: AgentError) -> IpcResponse {
    IpcResponse {
        id,
        ok: false,
        result: None,
        error: Some(error),
    }
}

pub fn generic_daemon_error() -> AgentError {
    AgentError {
        code: "daemon_error".into(),
        message: "Daemon returned an error without details.".into(),
        recoverable: true,
        next: format!(
            "Retry the command or inspect {}.",
            crate::paths::daemon_log_path().display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn record_start_params_queue_by_default() {
        let params: StartRecordingParams = serde_json::from_value(json!({})).unwrap();

        assert!(params.queue);
    }

    #[test]
    fn job_status_serializes_for_agents() {
        assert_eq!(
            serde_json::to_string(&JobStatus::Paused).unwrap(),
            "\"paused\""
        );
    }
}
