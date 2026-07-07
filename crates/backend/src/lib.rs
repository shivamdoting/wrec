use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use config::{save_config, store_path, AppConfig};
use domain::{
    CaptureSourceKind, CaptureTarget, Codec, FrameRate, Quality, RecorderEvent, RecorderMetrics,
    RecorderSettings, Resolution,
};
use store::{
    now_ms, CaptureDimensions, EventLevel, EventRecord, EventSource, MetricRecord, RecordingRecord,
    Store,
};

#[derive(Debug, Default, Clone)]
pub struct RecordingOverrides {
    pub source_kind: Option<CaptureSourceKind>,
    pub target_id: Option<u64>,
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

#[derive(Debug, Clone, Copy)]
pub enum ClientEventLevel {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Starting {
        session_id: u64,
        target: CaptureTarget,
        settings: RecorderSettings,
        output_path: PathBuf,
    },
    Log {
        session_id: Option<u64>,
        message: String,
        marked_started: bool,
    },
    Metrics {
        session_id: u64,
        metrics: RecorderMetrics,
    },
    Failed {
        recording_id: Option<u64>,
        message: String,
    },
    Exited {
        session_id: u64,
        success: bool,
        status: String,
        output_path: Option<PathBuf>,
    },
}

pub struct WrecBackend {
    store: Option<Store>,
    active_session_id: Option<u64>,
    active_output_path: Option<PathBuf>,
    failed_session_ids: HashSet<u64>,
}

impl WrecBackend {
    pub fn open() -> Self {
        let store = match Store::open(store_path()) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!("failed to open wrec store: {err}");
                None
            }
        };

        Self {
            store,
            active_session_id: None,
            active_output_path: None,
            failed_session_ids: HashSet::new(),
        }
    }

    pub fn active_session_id(&self) -> Option<u64> {
        self.active_session_id
    }

    pub fn active_output_path(&self) -> Option<&Path> {
        self.active_output_path.as_deref()
    }

    pub fn handle_recorder_event(&mut self, event: &RecorderEvent) -> BackendEvent {
        match event {
            RecorderEvent::Starting {
                session_id,
                target,
                settings,
                output_path,
            } => {
                self.active_session_id = Some(*session_id);
                self.active_output_path = Some(output_path.clone());
                self.failed_session_ids.remove(session_id);
                self.upsert_recording(*session_id, target, settings, output_path.clone());
                self.append_event(
                    Some(*session_id),
                    EventSource::Backend,
                    EventLevel::Info,
                    None,
                    format!("starting capture: {} ({:?})", target.name, target.kind),
                );

                BackendEvent::Starting {
                    session_id: *session_id,
                    target: target.clone(),
                    settings: settings.clone(),
                    output_path: output_path.clone(),
                }
            }
            RecorderEvent::Log {
                session_id,
                message,
            } => {
                let marked_started = message.contains("recording started");
                if marked_started {
                    if let Some(session_id) = session_id {
                        self.mark_recording_started(*session_id);
                    }
                }

                let dimensions = parse_capture_dimensions(message);
                if let (Some(session_id), Some(dimensions)) = (*session_id, dimensions) {
                    self.update_recording_dimensions(session_id, dimensions);
                }

                let source = recorder_event_source(message);
                self.append_event(*session_id, source, EventLevel::Info, None, message.clone());

                BackendEvent::Log {
                    session_id: *session_id,
                    message: message.clone(),
                    marked_started,
                }
            }
            RecorderEvent::Metrics {
                session_id,
                metrics,
            } => {
                self.append_metric(*session_id, metrics);

                BackendEvent::Metrics {
                    session_id: *session_id,
                    metrics: metrics.clone(),
                }
            }
            RecorderEvent::Failed {
                session_id,
                message,
            } => {
                let recording_id = session_id.or(self.active_session_id);
                if let Some(recording_id) = recording_id {
                    self.failed_session_ids.insert(recording_id);
                    self.mark_recording_failed(recording_id, message);
                }
                self.append_event(
                    recording_id,
                    EventSource::Backend,
                    EventLevel::Error,
                    None,
                    format!("error: {message}"),
                );
                self.active_session_id = None;
                self.active_output_path = None;

                BackendEvent::Failed {
                    recording_id,
                    message: message.clone(),
                }
            }
            RecorderEvent::Exited {
                session_id,
                success,
                status,
            } => {
                let output_path = self.active_output_path.clone();
                let failed_before_exit = self.failed_session_ids.remove(session_id);
                let success = *success && !failed_before_exit;

                if success {
                    self.mark_recording_completed(*session_id, output_path.as_deref());
                } else if !failed_before_exit {
                    self.mark_recording_failed(*session_id, status);
                }
                self.append_event(
                    Some(*session_id),
                    EventSource::Backend,
                    if success {
                        EventLevel::Info
                    } else {
                        EventLevel::Error
                    },
                    None,
                    format!("capture engine exited: {status}"),
                );
                self.active_session_id = None;
                self.active_output_path = None;

                BackendEvent::Exited {
                    session_id: *session_id,
                    success,
                    status: status.clone(),
                    output_path,
                }
            }
        }
    }

    fn append_event(
        &self,
        recording_id: Option<u64>,
        source: EventSource,
        level: EventLevel,
        fields_json: Option<String>,
        message: String,
    ) {
        if let Some(store) = &self.store {
            store.append_event(EventRecord {
                recording_id,
                timestamp_ms: now_ms(),
                level,
                source,
                message,
                fields_json,
            });
        }
    }

    pub fn append_app_event(
        &self,
        recording_id: Option<u64>,
        level: ClientEventLevel,
        message: String,
    ) {
        self.append_event(
            recording_id,
            EventSource::App,
            match level {
                ClientEventLevel::Info => EventLevel::Info,
                ClientEventLevel::Error => EventLevel::Error,
            },
            None,
            message,
        );
    }

    fn mark_recording_started(&self, session_id: u64) {
        if let Some(store) = &self.store {
            store.mark_recording_started(session_id);
        }
    }

    fn mark_recording_completed(&self, session_id: u64, output_path: Option<&Path>) {
        if let Some(store) = &self.store {
            let file_size = output_path
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len());
            store.mark_recording_completed(session_id, now_ms(), file_size);
        }
    }

    fn mark_recording_failed(&self, session_id: u64, message: &str) {
        if let Some(store) = &self.store {
            store.mark_recording_failed(session_id, now_ms(), message.to_string());
        }
    }

    fn upsert_recording(
        &self,
        session_id: u64,
        target: &CaptureTarget,
        settings: &RecorderSettings,
        output_path: PathBuf,
    ) {
        if let Some(store) = &self.store {
            store.upsert_recording(RecordingRecord {
                id: session_id,
                started_at_ms: now_ms(),
                output_path,
                target_kind: capture_kind_arg(target.kind).to_string(),
                target_id: target.id,
                target_name: target.name.clone(),
                codec: settings.codec.as_arg().to_string(),
                quality: settings.quality.as_arg().to_string(),
                resolution: settings.resolution.as_arg().to_string(),
                fps: settings.fps.as_u32(),
                include_cursor: settings.include_cursor,
                include_system_audio: settings.include_system_audio,
                include_microphone: settings.include_microphone,
            });
        }
    }

    fn update_recording_dimensions(&self, session_id: u64, dimensions: CaptureDimensions) {
        if let Some(store) = &self.store {
            store.update_dimensions(session_id, dimensions);
        }
    }

    fn append_metric(&self, session_id: u64, metrics: &RecorderMetrics) {
        if let Some(store) = &self.store {
            store.append_metric(MetricRecord {
                recording_id: session_id,
                timestamp_ms: now_ms(),
                elapsed_secs: metrics.elapsed_secs,
                output_bytes: metrics.output_bytes,
                bitrate_mbps: metrics.estimated_bitrate_mbps,
                frames: None,
                dropped_frames: None,
            });
        }
    }
}

pub fn load_config() -> AppConfig {
    AppConfig::load()
}

pub fn persist_config(config: &AppConfig) -> std::io::Result<()> {
    save_config(config)
}

pub fn build_settings(
    saved: &RecorderSettings,
    overrides: &RecordingOverrides,
) -> RecorderSettings {
    build_settings_report(saved, overrides).0
}

pub fn build_settings_report(
    saved: &RecorderSettings,
    overrides: &RecordingOverrides,
) -> (RecorderSettings, Option<String>) {
    let settings = settings_with_overrides(saved, overrides);
    let capped = settings.clone().with_preset_limits();
    let warning = preset_limit_warning(&settings, &capped);

    (capped, warning)
}

fn settings_with_overrides(
    saved: &RecorderSettings,
    overrides: &RecordingOverrides,
) -> RecorderSettings {
    let mut settings = saved.clone();

    if let Some(source) = overrides.source_kind {
        settings.source = source;
    }
    if let Some(fps) = overrides.fps {
        settings.fps = fps;
    }
    if let Some(codec) = overrides.codec {
        settings.codec = codec;
    }
    if let Some(quality) = overrides.quality {
        settings.quality = quality;
    }
    if let Some(resolution) = overrides.resolution {
        settings.resolution = resolution;
    }
    if let Some(output_dir) = overrides.output_dir.clone() {
        settings.output_dir = output_dir;
    }
    if let Some(include_cursor) = overrides.include_cursor {
        settings.include_cursor = include_cursor;
    }
    if let Some(include_system_audio) = overrides.include_system_audio {
        settings.include_system_audio = include_system_audio;
    }
    if let Some(include_microphone) = overrides.include_microphone {
        settings.include_microphone = include_microphone;
    }
    if let Some(hide_wrec) = overrides.hide_wrec {
        settings.hide_wrec = hide_wrec;
    }

    settings
}

fn preset_limit_warning(before: &RecorderSettings, after: &RecorderSettings) -> Option<String> {
    let mut changes = Vec::new();
    if before.fps != after.fps {
        changes.push(format!(
            "fps {} -> {}",
            before.fps.as_u32(),
            after.fps.as_u32()
        ));
    }
    if before.resolution != after.resolution {
        changes.push(format!(
            "resolution {} -> {}",
            before.resolution.as_arg(),
            after.resolution.as_arg()
        ));
    }

    (!changes.is_empty()).then(|| {
        format!(
            "preset limits applied: {} caps {}. Use --quality high to allow native/60 FPS.",
            after.quality.as_arg(),
            changes.join(", ")
        )
    })
}

pub fn selected_target_id(config: &AppConfig, kind: CaptureSourceKind) -> Option<u64> {
    let (selected_kind, id) = parse_target_key(config.selected_target_key.as_deref()?)?;
    (selected_kind == kind).then_some(id)
}

pub fn resolve_target(
    targets: &[CaptureTarget],
    kind: CaptureSourceKind,
    explicit_id: Option<u64>,
    saved_id: Option<u64>,
) -> Result<CaptureTarget, String> {
    if let Some(id) = explicit_id {
        return targets
            .iter()
            .find(|target| target.id == id && target.kind == kind)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "no {} with id {id}. Run `wrec targets --json` and pass one of the listed `{}` ids.",
                    capture_kind_arg(kind),
                    capture_kind_arg(kind)
                )
            });
    }

    if let Some(target) = saved_id.and_then(|id| {
        targets
            .iter()
            .find(|target| target.id == id && target.kind == kind)
            .cloned()
    }) {
        return Ok(target);
    }

    targets
        .iter()
        .find(|target| target.kind == kind)
        .cloned()
        .ok_or_else(|| {
            format!(
                "no {} capture targets available. Run `wrec targets --json` to inspect targets; if it fails, grant Screen Recording permission.",
                capture_kind_arg(kind)
            )
        })
}

pub fn capture_kind_arg(kind: CaptureSourceKind) -> &'static str {
    match kind {
        CaptureSourceKind::Display => "display",
        CaptureSourceKind::Window => "window",
    }
}

pub fn recorder_event_source(message: &str) -> EventSource {
    if message.starts_with("capture-engine:") {
        EventSource::CaptureEngine
    } else {
        EventSource::Backend
    }
}

pub fn parse_capture_dimensions(message: &str) -> Option<CaptureDimensions> {
    let (native_width, native_height) = parse_size_after(message, "native=")?;
    let (output_width, output_height) = parse_size_after(message, "size=")?;
    Some(CaptureDimensions {
        native_width,
        native_height,
        output_width,
        output_height,
    })
}

fn parse_target_key(key: &str) -> Option<(CaptureSourceKind, u64)> {
    let (kind, id) = key.split_once(':')?;
    let kind = match kind {
        "display" => CaptureSourceKind::Display,
        "window" => CaptureSourceKind::Window,
        _ => return None,
    };
    Some((kind, id.parse().ok()?))
}

fn parse_size_after(message: &str, key: &str) -> Option<(i64, i64)> {
    let token = message.split_once(key)?.1.split_whitespace().next()?;
    let (width, height) = token.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_keep_saved_output_dir_by_default() {
        let saved = RecorderSettings {
            output_dir: PathBuf::from("/tmp/chosen"),
            ..RecorderSettings::default()
        };

        assert_eq!(
            build_settings(&saved, &RecordingOverrides::default()).output_dir,
            PathBuf::from("/tmp/chosen")
        );
    }

    #[test]
    fn build_settings_enforces_preset_limits_for_cli_overrides() {
        let overrides = RecordingOverrides {
            quality: Some(Quality::Efficient),
            fps: Some(FrameRate::Fps60),
            resolution: Some(Resolution::Native),
            ..RecordingOverrides::default()
        };
        let settings = build_settings(&RecorderSettings::default(), &overrides);

        assert_eq!(settings.quality, Quality::Efficient);
        assert_eq!(settings.fps, FrameRate::Fps30);
        assert_eq!(settings.resolution, Resolution::R720p);
    }

    #[test]
    fn microphone_stays_off_unless_overridden() {
        let saved = RecorderSettings::default();

        assert!(!build_settings(&saved, &RecordingOverrides::default()).include_microphone);

        let overrides = RecordingOverrides {
            include_microphone: Some(true),
            ..RecordingOverrides::default()
        };
        assert!(build_settings(&saved, &overrides).include_microphone);
    }

    #[test]
    fn build_settings_report_describes_preset_limits() {
        let overrides = RecordingOverrides {
            quality: Some(Quality::Efficient),
            fps: Some(FrameRate::Fps60),
            resolution: Some(Resolution::Native),
            ..RecordingOverrides::default()
        };
        let (_, warning) = build_settings_report(&RecorderSettings::default(), &overrides);

        assert_eq!(
            warning.as_deref(),
            Some(
                "preset limits applied: efficient caps fps 60 -> 30, resolution native -> 720p. Use --quality high to allow native/60 FPS."
            )
        );
    }

    #[test]
    fn explicit_target_beats_saved_target() {
        let targets = vec![
            CaptureTarget {
                id: 1,
                name: "Saved".into(),
                kind: CaptureSourceKind::Display,
            },
            CaptureTarget {
                id: 2,
                name: "Explicit".into(),
                kind: CaptureSourceKind::Display,
            },
        ];

        let target =
            resolve_target(&targets, CaptureSourceKind::Display, Some(2), Some(1)).unwrap();
        assert_eq!(target.id, 2);
    }

    #[test]
    fn missing_explicit_target_error_is_actionable() {
        let err = resolve_target(&[], CaptureSourceKind::Display, Some(99), None).unwrap_err();

        assert_eq!(
            err,
            "no display with id 99. Run `wrec targets --json` and pass one of the listed `display` ids."
        );
    }

    #[test]
    fn parses_capture_dimensions_from_capture_engine_log() {
        let dimensions = parse_capture_dimensions(
            "capture-engine: recording started native=3024x1964 size=1512x982",
        )
        .unwrap();

        assert_eq!(dimensions.native_width, 3024);
        assert_eq!(dimensions.native_height, 1964);
        assert_eq!(dimensions.output_width, 1512);
        assert_eq!(dimensions.output_height, 982);
    }

    #[test]
    fn prior_failure_keeps_successful_exit_failed() {
        let target = CaptureTarget {
            id: 1,
            name: "Display".into(),
            kind: CaptureSourceKind::Display,
        };
        let mut backend = WrecBackend {
            store: None,
            active_session_id: None,
            active_output_path: None,
            failed_session_ids: HashSet::new(),
        };

        backend.handle_recorder_event(&RecorderEvent::Starting {
            session_id: 7,
            target,
            settings: RecorderSettings::default(),
            output_path: PathBuf::from("/tmp/wrec.mov"),
        });
        backend.handle_recorder_event(&RecorderEvent::Failed {
            session_id: Some(7),
            message: "writer failed".into(),
        });

        let event = backend.handle_recorder_event(&RecorderEvent::Exited {
            session_id: 7,
            success: true,
            status: "exit status: 0".into(),
        });

        assert!(matches!(event, BackendEvent::Exited { success: false, .. }));
    }
}
