use crate::{
    config::{save_config, AppConfig},
    platform::{choose_output_dir, open_path},
    ui::{
        fps_label, push_app_notification, target_key, AppTab, ControlSelect, TargetOption,
        TargetSelect, CODEC_OPTIONS, FPS_OPTIONS, QUALITY_OPTIONS, SOURCE_OPTIONS,
    },
};
use gpui::*;
use gpui_component::{
    input::{InputEvent, InputState},
    notification::Notification,
    select::{SelectEvent, SelectState},
    IndexPath,
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
use wrec_core::{
    CaptureSourceKind, CaptureTarget, Codec, FrameRate, Quality, RecorderEngine, RecorderMetrics,
    RecorderSettings, RecordingSession,
};
use wrec_macos::{MacosRecorder, RecorderEvent};

pub(crate) const GITHUB_URL: &str = "https://github.com/shivamhwp/wrec";

const MAX_LOGS: usize = 80;

#[derive(Clone, Debug)]
pub(crate) enum RecorderState {
    Idle,
    LoadingTargets,
    Starting,
    Recording,
    Stopping,
    Failed,
}

impl RecorderState {
    pub(crate) fn is_recording(&self) -> bool {
        matches!(self, Self::Recording)
    }

    pub(crate) fn is_busy(&self) -> bool {
        matches!(self, Self::LoadingTargets | Self::Starting | Self::Stopping)
    }
}

#[derive(Debug)]
enum AppEvent {
    TargetsLoaded(std::result::Result<Vec<CaptureTarget>, String>),
    Started(std::result::Result<RecordingSession, String>),
    Stopped(std::result::Result<(), String>),
}

pub(crate) struct WrecApp {
    engine: Arc<Mutex<MacosRecorder>>,
    app_events: mpsc::Sender<AppEvent>,
    pub(crate) settings: RecorderSettings,
    targets: Vec<CaptureTarget>,
    selected_target_key: Option<String>,
    active_session_id: Option<u64>,
    pub(crate) recorder_state: RecorderState,
    pub(crate) metrics: Option<RecorderMetrics>,
    status: String,
    pub(crate) active_tab: AppTab,
    last_recording_dir: Option<PathBuf>,
    logs: VecDeque<String>,
    pub(crate) source_select: Entity<ControlSelect>,
    pub(crate) target_select: Entity<TargetSelect>,
    pub(crate) codec_select: Entity<ControlSelect>,
    pub(crate) quality_select: Entity<ControlSelect>,
    pub(crate) fps_select: Entity<ControlSelect>,
    pub(crate) output_input: Entity<InputState>,
    _event_task: Task<()>,
}

impl WrecApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = AppConfig::load();
        let settings = config.settings;

        let (events, receiver) = mpsc::channel();
        let (app_events, app_receiver) = mpsc::channel();
        let event_task = cx.spawn_in(window, async move |this, cx| loop {
            while let Ok(event) = receiver.try_recv() {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_recorder_event(event, window, cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
            while let Ok(event) = app_receiver.try_recv() {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_app_event(event, window, cx);
                    })
                    .is_err()
                {
                    return;
                }
            }

            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
        });

        let source_select = cx.new(|cx| {
            SelectState::new(
                SOURCE_OPTIONS.to_vec(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let target_select =
            cx.new(|cx| SelectState::new(Vec::<TargetOption>::new(), None, window, cx));
        let codec_select = cx.new(|cx| {
            SelectState::new(
                CODEC_OPTIONS.to_vec(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let quality_select = cx.new(|cx| {
            SelectState::new(
                QUALITY_OPTIONS.to_vec(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let fps_select = cx.new(|cx| {
            SelectState::new(FPS_OPTIONS.to_vec(), Some(IndexPath::default()), window, cx)
        });
        let output_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.output_dir.display().to_string())
                .placeholder("Output folder")
        });

        cx.subscribe_in(&source_select, window, Self::on_source_select)
            .detach();
        cx.subscribe_in(&target_select, window, Self::on_target_select)
            .detach();
        cx.subscribe_in(&codec_select, window, Self::on_codec_select)
            .detach();
        cx.subscribe_in(&quality_select, window, Self::on_quality_select)
            .detach();
        cx.subscribe_in(&fps_select, window, Self::on_fps_select)
            .detach();
        cx.subscribe_in(&output_input, window, Self::on_output_input)
            .detach();

        fps_select.update(cx, |select, cx| {
            select.set_selected_value(&fps_label(settings.fps).into(), window, cx);
        });
        source_select.update(cx, |select, cx| {
            select.set_selected_value(&source_label(settings.source).into(), window, cx);
        });
        codec_select.update(cx, |select, cx| {
            select.set_selected_value(&codec_label(settings.codec).into(), window, cx);
        });
        quality_select.update(cx, |select, cx| {
            select.set_selected_value(&quality_label(settings.quality).into(), window, cx);
        });

        let mut app = Self {
            engine: Arc::new(Mutex::new(MacosRecorder::new(events))),
            app_events,
            settings,
            targets: Vec::new(),
            selected_target_key: config.selected_target_key,
            active_session_id: None,
            recorder_state: RecorderState::Idle,
            metrics: None,
            status: "Idle".to_string(),
            active_tab: AppTab::General,
            last_recording_dir: None,
            logs: VecDeque::new(),
            source_select,
            target_select,
            codec_select,
            quality_select,
            fps_select,
            output_input,
            _event_task: event_task,
        };
        app.refresh_targets(cx);
        app
    }

    fn on_source_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.settings.source = match *value {
            "Window" => CaptureSourceKind::Window,
            _ => CaptureSourceKind::Display,
        };
        self.selected_target_key = None;
        self.sync_target_select(window, cx);
        self.push_log(format!("source: {value}"));
        self.save_config();
        cx.notify();
    }

    fn on_target_select(
        &mut self,
        _: &Entity<TargetSelect>,
        event: &SelectEvent<Vec<TargetOption>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.selected_target_key = Some(value.to_string());
        if let Some(target) = self.selected_target() {
            self.push_log(format!("target: {}", target.name));
        }
        self.save_config();
        cx.notify();
    }

    fn on_codec_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.settings.codec = match *value {
            "H.264" => Codec::H264,
            _ => Codec::Hevc,
        };
        self.push_log(format!("codec: {value}"));
        self.save_config();
        cx.notify();
    }

    fn on_quality_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.settings.quality = match *value {
            "Efficient" => Quality::Efficient,
            "High" => Quality::High,
            _ => Quality::Balanced,
        };
        self.push_log(format!("quality: {value}"));
        self.save_config();
        cx.notify();
    }

    fn on_fps_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        let fps = match *value {
            "60 FPS" => FrameRate::Fps60,
            _ => FrameRate::Fps30,
        };
        self.set_fps(fps, cx);
    }

    fn on_output_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change | InputEvent::PressEnter { .. }) {
            let value = input.read(cx).value().trim().to_string();
            if !value.is_empty() {
                self.settings.output_dir = PathBuf::from(value);
                self.push_log(format!(
                    "output dir: {}",
                    self.settings.output_dir.display()
                ));
                self.save_config();
                cx.notify();
            }
        }
    }

    fn set_fps(&mut self, fps: FrameRate, cx: &mut Context<Self>) {
        self.settings.fps = fps;
        self.push_log(format!("fps: {}", fps.as_u32()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_include_cursor(&mut self, include_cursor: bool, cx: &mut Context<Self>) {
        self.settings.include_cursor = include_cursor;
        self.push_log(format!(
            "cursor: {}",
            if include_cursor { "on" } else { "off" }
        ));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn choose_output_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = choose_output_dir() else {
            self.push_log("output picker cancelled");
            return;
        };

        self.settings.output_dir = path.clone();
        let value = path.display().to_string();
        self.output_input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
        });
        self.push_log(format!("output dir: {value}"));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn refresh_targets(&mut self, cx: &mut Context<Self>) {
        if self.recorder_state.is_busy() || self.recorder_state.is_recording() {
            return;
        }

        self.recorder_state = RecorderState::LoadingTargets;
        self.status = "Loading capture targets".to_string();
        self.push_log("loading capture targets");
        let engine = self.engine.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = engine
                .lock()
                .unwrap()
                .list_targets()
                .map_err(|err| err.to_string());
            let _ = app_events.send(AppEvent::TargetsLoaded(result));
        });
        cx.notify();
    }

    fn sync_target_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = self
            .targets
            .iter()
            .filter(|target| target.kind == self.settings.source)
            .map(TargetOption::new)
            .collect::<Vec<_>>();

        let selected_key = self
            .selected_target_key
            .clone()
            .filter(|key| options.iter().any(|option| option.key().as_ref() == key))
            .or_else(|| options.first().map(|option| option.key().to_string()));

        self.selected_target_key = selected_key.clone();
        self.target_select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            if let Some(key) = selected_key {
                select.set_selected_value(&key.into(), window, cx);
            } else {
                select.set_selected_index(None, window, cx);
            }
        });
    }

    fn selected_target(&self) -> Option<CaptureTarget> {
        self.selected_target_key
            .as_ref()
            .and_then(|key| {
                self.targets
                    .iter()
                    .find(|target| target_key(target) == *key)
                    .cloned()
            })
            .or_else(|| {
                self.targets
                    .iter()
                    .find(|target| target.kind == self.settings.source)
                    .cloned()
            })
    }

    pub(crate) fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recorder_state.is_busy() {
            self.push_log("recorder is busy");
            return;
        }

        if self.recorder_state.is_recording() {
            self.recorder_state = RecorderState::Stopping;
            self.status = "Stopping".to_string();
            self.push_log("stopping recording");
            let engine = self.engine.clone();
            let app_events = self.app_events.clone();
            std::thread::spawn(move || {
                let result = engine.lock().unwrap().stop().map_err(|err| err.to_string());
                let _ = app_events.send(AppEvent::Stopped(result));
            });
            cx.notify();
            return;
        }

        let Some(target) = self.selected_target() else {
            self.show_error("No capture target selected", window, cx);
            return;
        };

        self.recorder_state = RecorderState::Starting;
        self.status = format!("Starting {}", target.name);
        self.metrics = None;
        self.push_log(format!("target: {}", target.name));
        let engine = self.engine.clone();
        let app_events = self.app_events.clone();
        let settings = self.settings.clone();
        std::thread::spawn(move || {
            let result = engine
                .lock()
                .unwrap()
                .start(target, settings)
                .map_err(|err| err.to_string());
            let _ = app_events.send(AppEvent::Started(result));
        });
        cx.notify();
    }

    fn handle_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::TargetsLoaded(Ok(targets)) => {
                let count = targets.len();
                self.targets = targets;
                self.sync_target_select(window, cx);
                self.recorder_state = RecorderState::Idle;
                let message = format!("{count} capture targets loaded");
                self.status = "Idle".to_string();
                self.push_log(message.clone());
                push_app_notification(window, Notification::new().message(message), cx);
            }
            AppEvent::TargetsLoaded(Err(message)) => {
                self.recorder_state = RecorderState::Failed;
                self.show_error(message, window, cx);
            }
            AppEvent::Started(Ok(session)) => {
                self.active_session_id = Some(session.id);
                self.last_recording_dir = session.output_path.parent().map(Path::to_path_buf);
                self.status = format!("Recording to {}", session.output_path.display());
                self.recorder_state = RecorderState::Recording;
                self.push_log(self.status.clone());
                push_app_notification(window, Notification::new().message("Recording started"), cx);
            }
            AppEvent::Started(Err(message)) => {
                self.active_session_id = None;
                self.recorder_state = RecorderState::Failed;
                self.show_error(message, window, cx);
            }
            AppEvent::Stopped(Ok(())) => {
                self.active_session_id = None;
                self.recorder_state = RecorderState::Idle;
                self.status = "Stopped".to_string();
                self.push_log("Stopped");
                if let Some(path) = self.last_recording_dir.clone() {
                    match open_path(&path) {
                        Ok(()) => self.push_log(format!("opened: {}", path.display())),
                        Err(err) => {
                            self.push_log(format!("open failed: {err}"));
                            push_app_notification(
                                window,
                                Notification::new()
                                    .message(format!("Could not open output folder: {err}")),
                                cx,
                            );
                        }
                    }
                }
                push_app_notification(window, Notification::new().message("Recording stopped"), cx);
            }
            AppEvent::Stopped(Err(message)) => {
                self.active_session_id = None;
                self.recorder_state = RecorderState::Failed;
                self.show_error(message, window, cx);
            }
        }
    }

    fn handle_recorder_event(
        &mut self,
        event: RecorderEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            RecorderEvent::Log {
                session_id,
                message,
            } => {
                if !self.should_accept_recorder_event(session_id) {
                    return;
                }
                if message.contains("recording started") {
                    self.status = "Recording".to_string();
                }
                self.push_log(message);
            }
            RecorderEvent::Metrics {
                session_id,
                metrics,
            } => {
                if !self.should_accept_recorder_event(Some(session_id)) {
                    return;
                }
                self.metrics = Some(metrics);
                cx.notify();
            }
            RecorderEvent::Failed {
                session_id,
                message,
            } => {
                if !self.should_accept_recorder_event(session_id) {
                    return;
                }
                self.active_session_id = None;
                self.recorder_state = RecorderState::Failed;
                self.show_error(message, window, cx);
            }
            RecorderEvent::Exited {
                session_id,
                success,
                status,
            } => {
                if !self.should_accept_recorder_event(Some(session_id)) {
                    return;
                }
                self.active_session_id = None;
                self.recorder_state = if success {
                    RecorderState::Idle
                } else {
                    RecorderState::Failed
                };
                if success {
                    self.push_log(format!("helper exited: {status}"));
                } else {
                    self.show_error(format!("helper exited: {status}"), window, cx);
                }
            }
        }
    }

    fn should_accept_recorder_event(&self, session_id: Option<u64>) -> bool {
        match (session_id, self.active_session_id) {
            (None, _) => true,
            (Some(event_session), Some(active_session)) => event_session == active_session,
            (Some(_), None) => matches!(self.recorder_state, RecorderState::Starting),
        }
    }

    fn show_error(
        &mut self,
        message: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = message.into();
        self.status = message.clone();
        self.push_log(format!("error: {message}"));
        tracing::error!("{message}");
        push_app_notification(
            window,
            Notification::new().message(message).autohide(false),
            cx,
        );
    }

    pub(crate) fn push_log(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::info!("{message}");
        self.logs.push_back(message);
        while self.logs.len() > MAX_LOGS {
            self.logs.pop_front();
        }
    }

    fn save_config(&mut self) {
        let config = AppConfig {
            settings: self.settings.clone(),
            selected_target_key: self.selected_target_key.clone(),
        };

        if let Err(err) = save_config(&config) {
            self.push_log(format!("config save failed: {err}"));
            tracing::warn!("failed to save config: {err}");
        }
    }
}

fn source_label(source: CaptureSourceKind) -> &'static str {
    match source {
        CaptureSourceKind::Window => "Window",
        CaptureSourceKind::Display => "Display",
    }
}

fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
    }
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Efficient => "Efficient",
        Quality::High => "High",
        Quality::Balanced => "Balanced",
    }
}
