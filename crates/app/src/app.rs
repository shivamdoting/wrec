use crate::{
    config::{save_config, store_path, wrec_dir, AppConfig},
    platform::{choose_output_dir, open_path},
    ui::{
        fps_label, output_format_label, push_app_notification, resolution_label, target_key,
        AppTab, ControlSelect, TargetOption, TargetSelect, CODEC_OPTIONS, FORMAT_OPTIONS,
        FPS_OPTIONS, QUALITY_OPTIONS, RESOLUTION_OPTIONS, SOURCE_OPTIONS,
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
    CaptureSourceKind, CaptureTarget, Codec, FrameRate, OutputFormat, Quality, RecorderEngine,
    RecorderMetrics, RecorderSettings, RecordingSession, Resolution,
    ScreenRecordingPermissionStatus,
};
use wrec_macos::{MacosRecorder, RecorderEvent};
use wrec_store::{
    now_ms, CaptureDimensions, EventLevel, EventRecord, EventSource, MetricRecord, RecordingRecord,
    Store,
};

pub(crate) const GITHUB_URL: &str = "https://github.com/shivamhwp/wrec";

const MAX_LOGS: usize = 80;

#[derive(Clone, Debug)]
pub(crate) enum RecorderState {
    Idle,
    LoadingTargets,
    Starting,
    Recording,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    Failed,
}

impl RecorderState {
    pub(crate) fn is_recording(&self) -> bool {
        matches!(self, Self::Recording)
    }

    pub(crate) fn is_paused(&self) -> bool {
        matches!(self, Self::Paused)
    }

    pub(crate) fn is_active_session(&self) -> bool {
        matches!(
            self,
            Self::Recording | Self::Pausing | Self::Paused | Self::Resuming | Self::Stopping
        )
    }

    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::LoadingTargets | Self::Starting | Self::Pausing | Self::Resuming | Self::Stopping
        )
    }
}

#[derive(Debug)]
enum AppEvent {
    PermissionChecked {
        result: std::result::Result<ScreenRecordingPermissionStatus, String>,
        refresh_targets_after_granted: bool,
    },
    PermissionRequested(std::result::Result<ScreenRecordingPermissionStatus, String>),
    TargetsLoaded(std::result::Result<Vec<CaptureTarget>, String>),
    Started(std::result::Result<RecordingSession, String>),
    Paused(std::result::Result<(), String>),
    Resumed(std::result::Result<(), String>),
    Stopped(std::result::Result<(), String>),
}

pub(crate) struct WrecApp {
    engine: Arc<Mutex<MacosRecorder>>,
    store: Option<Store>,
    app_events: mpsc::Sender<AppEvent>,
    pub(crate) settings: RecorderSettings,
    targets: Vec<CaptureTarget>,
    selected_target_key: Option<String>,
    active_session_id: Option<u64>,
    active_output_path: Option<PathBuf>,
    pub(crate) recorder_state: RecorderState,
    pub(crate) permission_status: ScreenRecordingPermissionStatus,
    pub(crate) permission_busy: bool,
    pub(crate) metrics: Option<RecorderMetrics>,
    pub(crate) status: String,
    pub(crate) active_tab: AppTab,
    pub(crate) last_recording_dir: Option<PathBuf>,
    pub(crate) show_nerd_logs: bool,
    pub(crate) logs: VecDeque<String>,
    pub(crate) source_select: Entity<ControlSelect>,
    pub(crate) target_select: Entity<TargetSelect>,
    pub(crate) output_format_select: Entity<ControlSelect>,
    pub(crate) codec_select: Entity<ControlSelect>,
    pub(crate) quality_select: Entity<ControlSelect>,
    pub(crate) resolution_select: Entity<ControlSelect>,
    pub(crate) fps_select: Entity<ControlSelect>,
    pub(crate) output_input: Entity<InputState>,
    _event_task: Task<()>,
}

impl WrecApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = AppConfig::load();
        let settings = config.settings;
        let store = match Store::open(store_path()) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!("failed to open wrec store: {err}");
                None
            }
        };

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
        let output_format_select = cx.new(|cx| {
            SelectState::new(
                FORMAT_OPTIONS.to_vec(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
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
        let resolution_select = cx.new(|cx| {
            SelectState::new(
                RESOLUTION_OPTIONS.to_vec(),
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
        cx.subscribe_in(&output_format_select, window, Self::on_output_format_select)
            .detach();
        cx.subscribe_in(&codec_select, window, Self::on_codec_select)
            .detach();
        cx.subscribe_in(&quality_select, window, Self::on_quality_select)
            .detach();
        cx.subscribe_in(&resolution_select, window, Self::on_resolution_select)
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
        output_format_select.update(cx, |select, cx| {
            select.set_selected_value(
                &output_format_label(settings.output_format).into(),
                window,
                cx,
            );
        });
        codec_select.update(cx, |select, cx| {
            select.set_selected_value(&codec_label(settings.codec).into(), window, cx);
        });
        quality_select.update(cx, |select, cx| {
            select.set_selected_value(&quality_label(settings.quality).into(), window, cx);
        });
        resolution_select.update(cx, |select, cx| {
            select.set_selected_value(&resolution_label(settings.resolution).into(), window, cx);
        });

        let mut app = Self {
            engine: Arc::new(Mutex::new(MacosRecorder::new(events))),
            store,
            app_events,
            settings,
            targets: Vec::new(),
            selected_target_key: config.selected_target_key,
            active_session_id: None,
            active_output_path: None,
            recorder_state: RecorderState::Idle,
            permission_status: ScreenRecordingPermissionStatus::Unknown,
            permission_busy: false,
            metrics: None,
            status: "Idle".to_string(),
            active_tab: AppTab::General,
            last_recording_dir: None,
            show_nerd_logs: config.show_nerd_logs,
            logs: VecDeque::new(),
            source_select,
            target_select,
            output_format_select,
            codec_select,
            quality_select,
            resolution_select,
            fps_select,
            output_input,
            _event_task: event_task,
        };
        app.refresh_permission_status(true, cx);
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

    fn on_output_format_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.settings.output_format = match *value {
            "GIF" => OutputFormat::Gif,
            _ => OutputFormat::Mov,
        };
        self.push_log(format!("format: {value}"));
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

    fn on_resolution_select(
        &mut self,
        _: &Entity<ControlSelect>,
        event: &SelectEvent<Vec<&'static str>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.settings.resolution = match *value {
            "720p" => Resolution::R720p,
            "1080p" => Resolution::R1080p,
            "2K" => Resolution::R2k,
            "4K" => Resolution::R4k,
            _ => Resolution::Native,
        };
        self.push_log(format!("resolution: {value}"));
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

    pub(crate) fn set_include_system_audio(
        &mut self,
        include_system_audio: bool,
        cx: &mut Context<Self>,
    ) {
        self.settings.include_system_audio = include_system_audio;
        self.push_log(format!(
            "system audio: {}",
            if include_system_audio { "on" } else { "off" }
        ));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_hide_wrec(&mut self, hide_wrec: bool, cx: &mut Context<Self>) {
        self.settings.hide_wrec = hide_wrec;
        self.push_log(format!(
            "hide wrec: {}",
            if hide_wrec { "on" } else { "off" }
        ));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_show_nerd_logs(&mut self, show_nerd_logs: bool, cx: &mut Context<Self>) {
        self.show_nerd_logs = show_nerd_logs;
        if show_nerd_logs {
            self.active_tab = AppTab::Nerd;
        } else if self.active_tab == AppTab::Nerd {
            self.active_tab = AppTab::Settings;
        }
        self.push_log(format!(
            "nerd logs: {}",
            if show_nerd_logs { "on" } else { "off" }
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

    pub(crate) fn open_last_recording_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.last_recording_dir.clone() else {
            return;
        };

        match open_path(&path) {
            Ok(()) => self.push_log(format!("opened: {}", path.display())),
            Err(err) => {
                self.push_log(format!("open failed: {err}"));
                push_app_notification(
                    window,
                    Notification::new().message(format!("Could not open output folder: {err}")),
                    cx,
                );
            }
        }
        cx.notify();
    }

    pub(crate) fn open_recordings_data_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = wrec_dir();
        if let Err(err) = std::fs::create_dir_all(&path) {
            self.push_log(format!("recordings data folder create failed: {err}"));
            push_app_notification(
                window,
                Notification::new()
                    .message(format!("Could not create recordings data folder: {err}")),
                cx,
            );
            cx.notify();
            return;
        }

        match open_path(&path) {
            Ok(()) => self.push_log(format!("opened recordings data folder: {}", path.display())),
            Err(err) => {
                self.push_log(format!("recordings data folder open failed: {err}"));
                push_app_notification(
                    window,
                    Notification::new()
                        .message(format!("Could not open recordings data folder: {err}")),
                    cx,
                );
            }
        }
        cx.notify();
    }

    pub(crate) fn refresh_permission_status(
        &mut self,
        refresh_targets_after_granted: bool,
        cx: &mut Context<Self>,
    ) {
        if self.permission_busy {
            return;
        }

        self.permission_busy = true;
        self.status = "Checking Screen Recording permission".to_string();
        self.push_log("checking Screen Recording permission");
        let engine = self.engine.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = engine
                .lock()
                .unwrap()
                .screen_recording_permission_status()
                .map_err(|err| err.to_string());
            let _ = app_events.send(AppEvent::PermissionChecked {
                result,
                refresh_targets_after_granted,
            });
        });
        cx.notify();
    }

    pub(crate) fn request_screen_recording_permission(&mut self, cx: &mut Context<Self>) {
        if self.permission_busy {
            return;
        }

        self.permission_busy = true;
        self.status = "Requesting Screen Recording permission".to_string();
        self.push_log("requesting Screen Recording permission");
        let engine = self.engine.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = engine
                .lock()
                .unwrap()
                .request_screen_recording_permission()
                .map_err(|err| err.to_string());
            let _ = app_events.send(AppEvent::PermissionRequested(result));
        });
        cx.notify();
    }

    pub(crate) fn refresh_targets(&mut self, cx: &mut Context<Self>) {
        if self.recorder_state.is_busy() || self.recorder_state.is_recording() {
            return;
        }

        if !self.permission_status.is_granted() {
            self.push_log("target refresh skipped: Screen Recording permission missing");
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

    pub(crate) fn selected_target(&self) -> Option<CaptureTarget> {
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
        if self.recorder_state.is_busy() && !self.recorder_state.is_active_session() {
            self.push_log("recorder is busy");
            return;
        }

        if self.recorder_state.is_active_session() {
            if self.recorder_state.is_busy() {
                self.push_log("recorder is busy");
                return;
            }
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

        if !self.permission_status.is_granted() {
            self.show_error("Screen Recording permission is required", window, cx);
            return;
        }

        let Some(target) = self.selected_target() else {
            self.show_error("No capture target selected", window, cx);
            return;
        };

        self.start_recording(target, cx);
    }

    fn start_recording(&mut self, target: CaptureTarget, cx: &mut Context<Self>) {
        self.recorder_state = RecorderState::Starting;
        self.status = format!("Starting {}", target.name);
        self.metrics = None;
        self.push_log(format!("target: {}", target.name));
        if self.settings.hide_wrec {
            self.push_log("hiding Wrec from recording");
        }
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

    pub(crate) fn toggle_pause(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.recorder_state {
            RecorderState::Recording => {
                self.recorder_state = RecorderState::Pausing;
                self.status = "Pausing".to_string();
                self.push_log("pausing recording");
                let engine = self.engine.clone();
                let app_events = self.app_events.clone();
                std::thread::spawn(move || {
                    let result = engine
                        .lock()
                        .unwrap()
                        .pause()
                        .map_err(|err| err.to_string());
                    let _ = app_events.send(AppEvent::Paused(result));
                });
                cx.notify();
            }
            RecorderState::Paused => {
                self.recorder_state = RecorderState::Resuming;
                self.status = "Resuming".to_string();
                self.push_log("resuming recording");
                let engine = self.engine.clone();
                let app_events = self.app_events.clone();
                std::thread::spawn(move || {
                    let result = engine
                        .lock()
                        .unwrap()
                        .resume()
                        .map_err(|err| err.to_string());
                    let _ = app_events.send(AppEvent::Resumed(result));
                });
                cx.notify();
            }
            _ => self.show_error("Recording is not ready to pause or resume", window, cx),
        }
    }

    fn handle_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            AppEvent::PermissionChecked {
                result: Ok(status),
                refresh_targets_after_granted,
            } => {
                self.permission_busy = false;
                self.permission_status = status;
                match status {
                    ScreenRecordingPermissionStatus::Granted => {
                        self.status = "Ready".to_string();
                        self.push_log("Screen Recording permission granted");
                        if refresh_targets_after_granted {
                            self.refresh_targets(cx);
                            return;
                        }
                    }
                    ScreenRecordingPermissionStatus::Missing => {
                        self.targets.clear();
                        self.sync_target_select(window, cx);
                        self.status = "Screen Recording permission needed".to_string();
                        self.push_log("Screen Recording permission missing");
                    }
                    ScreenRecordingPermissionStatus::Unknown => {
                        self.status = "Screen Recording permission unknown".to_string();
                        self.push_log("Screen Recording permission unknown");
                    }
                }
                cx.notify();
            }
            AppEvent::PermissionChecked {
                result: Err(message),
                ..
            } => {
                self.permission_busy = false;
                self.permission_status = ScreenRecordingPermissionStatus::Unknown;
                self.show_error(message, window, cx);
            }
            AppEvent::PermissionRequested(Ok(status)) => {
                self.permission_busy = false;
                self.permission_status = status;
                match status {
                    ScreenRecordingPermissionStatus::Granted => {
                        self.status = "Ready".to_string();
                        self.push_log("Screen Recording permission granted");
                        push_app_notification(
                            window,
                            Notification::new().message(
                                "Screen Recording permission granted. Refresh targets to continue.",
                            ),
                            cx,
                        );
                        cx.notify();
                    }
                    ScreenRecordingPermissionStatus::Missing => {
                        self.status = "Screen Recording permission needed".to_string();
                        self.push_log("Screen Recording permission still missing");
                        push_app_notification(
                            window,
                            Notification::new()
                                .message("Screen Recording permission is still missing")
                                .autohide(false),
                            cx,
                        );
                        cx.notify();
                    }
                    ScreenRecordingPermissionStatus::Unknown => {
                        self.status = "Screen Recording permission unknown".to_string();
                        self.push_log("Screen Recording permission unknown");
                        cx.notify();
                    }
                }
            }
            AppEvent::PermissionRequested(Err(message)) => {
                self.permission_busy = false;
                self.permission_status = ScreenRecordingPermissionStatus::Unknown;
                self.show_error(message, window, cx);
            }
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
                if is_permission_message(&message) {
                    self.permission_status = ScreenRecordingPermissionStatus::Missing;
                }
                self.show_error(message, window, cx);
            }
            AppEvent::Started(Ok(session)) => {
                if !matches!(self.recorder_state, RecorderState::Starting) {
                    self.push_log(format!(
                        "ignored late start: {}",
                        session.output_path.display()
                    ));
                    cx.notify();
                    return;
                }
                self.active_session_id = Some(session.id);
                self.active_output_path = Some(session.output_path.clone());
                self.mark_recording_started(session.id);
                self.last_recording_dir = session.output_path.parent().map(Path::to_path_buf);
                self.status = format!("Recording to {}", session.output_path.display());
                self.recorder_state = RecorderState::Recording;
                self.push_log(self.status.clone());
                push_app_notification(window, Notification::new().message("Recording started"), cx);
            }
            AppEvent::Started(Err(message)) => {
                if !matches!(self.recorder_state, RecorderState::Starting) {
                    self.push_log(format!("ignored late start failure: {message}"));
                    cx.notify();
                    return;
                }
                let recording_id = self.active_session_id;
                if let Some(recording_id) = recording_id {
                    self.mark_recording_failed(recording_id, &message);
                }
                self.active_session_id = None;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                if is_permission_message(&message) {
                    self.permission_status = ScreenRecordingPermissionStatus::Missing;
                }
                cx.activate(true);
                window.activate_window();
                self.show_error_for(recording_id, message, window, cx);
            }
            AppEvent::Paused(Ok(())) => {
                if !matches!(self.recorder_state, RecorderState::Pausing) {
                    cx.notify();
                    return;
                }
                self.recorder_state = RecorderState::Paused;
                self.status = "Paused".to_string();
                self.push_log("Paused");
                cx.notify();
            }
            AppEvent::Paused(Err(message)) => {
                if matches!(self.recorder_state, RecorderState::Pausing) {
                    self.recorder_state = RecorderState::Recording;
                }
                self.show_error(message, window, cx);
            }
            AppEvent::Resumed(Ok(())) => {
                if !matches!(self.recorder_state, RecorderState::Resuming) {
                    cx.notify();
                    return;
                }
                self.recorder_state = RecorderState::Recording;
                self.status = "Recording".to_string();
                self.push_log("Resumed");
                cx.notify();
            }
            AppEvent::Resumed(Err(message)) => {
                if matches!(self.recorder_state, RecorderState::Resuming) {
                    self.recorder_state = RecorderState::Paused;
                }
                self.show_error(message, window, cx);
            }
            AppEvent::Stopped(Ok(())) => {
                let recording_id = self.active_session_id;
                let output_path = self.active_output_path.clone();
                if let Some(recording_id) = recording_id {
                    self.mark_recording_completed(recording_id, output_path.as_deref());
                }
                self.active_session_id = None;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Idle;
                self.status = "Stopped".to_string();
                self.push_log_for(recording_id, EventSource::App, EventLevel::Info, "Stopped");
                cx.activate(true);
                window.activate_window();
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
                let recording_id = self.active_session_id;
                if let Some(recording_id) = recording_id {
                    self.mark_recording_failed(recording_id, &message);
                }
                self.active_session_id = None;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                cx.activate(true);
                window.activate_window();
                self.show_error_for(recording_id, message, window, cx);
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
            RecorderEvent::Starting {
                session_id,
                target,
                settings,
                output_path,
            } => {
                if !self.should_accept_recorder_event(Some(session_id)) {
                    return;
                }
                self.active_session_id = Some(session_id);
                self.active_output_path = Some(output_path.clone());
                self.last_recording_dir = output_path.parent().map(Path::to_path_buf);
                self.upsert_recording(session_id, &target, &settings, output_path);
                self.push_log_for(
                    Some(session_id),
                    EventSource::Backend,
                    EventLevel::Info,
                    format!("starting capture: {} ({:?})", target.name, target.kind),
                );
            }
            RecorderEvent::Log {
                session_id,
                message,
            } => {
                if !self.should_accept_recorder_event(session_id) {
                    return;
                }
                if message.contains("recording started") {
                    self.status = "Recording".to_string();
                    if let Some(session_id) = session_id {
                        self.mark_recording_started(session_id);
                    }
                }
                if let (Some(session_id), Some(dimensions)) =
                    (session_id, parse_capture_dimensions(&message))
                {
                    self.update_recording_dimensions(session_id, dimensions);
                }
                self.push_log_for(
                    session_id,
                    recorder_event_source(&message),
                    EventLevel::Info,
                    message,
                );
            }
            RecorderEvent::Metrics {
                session_id,
                metrics,
            } => {
                if !self.should_accept_recorder_event(Some(session_id)) {
                    return;
                }
                self.append_metric(session_id, &metrics);
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
                let recording_id = session_id.or(self.active_session_id);
                if let Some(recording_id) = recording_id {
                    self.mark_recording_failed(recording_id, &message);
                }
                self.active_session_id = None;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                if is_permission_message(&message) {
                    self.permission_status = ScreenRecordingPermissionStatus::Missing;
                }
                cx.activate(true);
                window.activate_window();
                self.show_error_for(recording_id, message, window, cx);
            }
            RecorderEvent::Exited {
                session_id,
                success,
                status,
            } => {
                if !self.should_accept_recorder_event(Some(session_id)) {
                    return;
                }
                if !success {
                    self.mark_recording_failed(session_id, &status);
                }
                self.active_session_id = None;
                self.active_output_path = None;
                self.recorder_state = if success {
                    RecorderState::Idle
                } else {
                    RecorderState::Failed
                };
                cx.activate(true);
                window.activate_window();
                if success {
                    self.push_log_for(
                        Some(session_id),
                        EventSource::Backend,
                        EventLevel::Info,
                        format!("helper exited: {status}"),
                    );
                } else {
                    self.show_error_for(
                        Some(session_id),
                        format!("helper exited: {status}"),
                        window,
                        cx,
                    );
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
        self.show_error_for(self.active_session_id, message, window, cx);
    }

    fn show_error_for(
        &mut self,
        recording_id: Option<u64>,
        message: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = message.into();
        self.status = message.clone();
        self.push_log_for(
            recording_id,
            EventSource::App,
            EventLevel::Error,
            format!("error: {message}"),
        );
        tracing::error!("{message}");
        push_app_notification(
            window,
            Notification::new().message(message).autohide(false),
            cx,
        );
    }

    pub(crate) fn push_log(&mut self, message: impl Into<String>) {
        self.push_log_for(
            self.active_session_id,
            EventSource::App,
            EventLevel::Info,
            message,
        );
    }

    fn push_log_for(
        &mut self,
        recording_id: Option<u64>,
        source: EventSource,
        level: EventLevel,
        message: impl Into<String>,
    ) {
        let message = message.into();
        tracing::info!("{message}");
        self.logs.push_back(message);
        while self.logs.len() > MAX_LOGS {
            self.logs.pop_front();
        }
        self.append_event(
            recording_id,
            source,
            level,
            None,
            self.logs.back().unwrap().clone(),
        );
    }

    fn save_config(&mut self) {
        let config = AppConfig {
            settings: self.settings.clone(),
            selected_target_key: self.selected_target_key.clone(),
            show_nerd_logs: self.show_nerd_logs,
        };

        if let Err(err) = save_config(&config) {
            self.push_log(format!("config save failed: {err}"));
            tracing::warn!("failed to save config: {err}");
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
                codec: recording_codec(settings).to_string(),
                quality: settings.quality.as_arg().to_string(),
                resolution: settings.resolution.as_arg().to_string(),
                fps: settings.fps.as_u32(),
                include_cursor: settings.include_cursor,
                include_system_audio: settings.include_system_audio,
            });
        }
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

    fn update_recording_dimensions(&self, session_id: u64, dimensions: CaptureDimensions) {
        if let Some(store) = &self.store {
            store.update_dimensions(session_id, dimensions);
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

fn recording_codec(settings: &RecorderSettings) -> &'static str {
    match settings.output_format {
        OutputFormat::Gif => "gif",
        OutputFormat::Mov => settings.codec.as_arg(),
    }
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Efficient => "Efficient",
        Quality::High => "High",
        Quality::Balanced => "Balanced",
    }
}

fn is_permission_message(message: &str) -> bool {
    message.contains("Screen Recording") || message.contains("screen recording permission")
}

fn capture_kind_arg(kind: CaptureSourceKind) -> &'static str {
    match kind {
        CaptureSourceKind::Display => "display",
        CaptureSourceKind::Window => "window",
    }
}

fn recorder_event_source(message: &str) -> EventSource {
    if message.starts_with("wrec-helper:") {
        EventSource::Helper
    } else {
        EventSource::Backend
    }
}

fn parse_capture_dimensions(message: &str) -> Option<CaptureDimensions> {
    let (native_width, native_height) = parse_size_after(message, "native=")?;
    let (output_width, output_height) = parse_size_after(message, "size=")?;
    Some(CaptureDimensions {
        native_width,
        native_height,
        output_width,
        output_height,
    })
}

fn parse_size_after(message: &str, key: &str) -> Option<(i64, i64)> {
    let token = message.split_once(key)?.1.split_whitespace().next()?;
    let (width, height) = token.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}
