use crate::{
    platform::{choose_output_dir, open_path, CliInstallStatus},
    ui::{
        fps_disabled, fps_label, fps_options_for, push_app_notification, resolution_disabled,
        resolution_label, resolution_options_for, target_key, AppTab, ControlSelect, LimitedOption,
        LimitedSelect, TargetOption, TargetSelect, CODEC_OPTIONS, QUALITY_OPTIONS, SOURCE_OPTIONS,
    },
};
use config::{save_config as persist_config, wrec_dir, AppConfig};
use control::{
    AgentError, DaemonClient, EventLevel, JobSnapshot, JobStatus, RecordingOptions,
    StartRecordingParams, TargetSelector,
};
use domain::{
    CaptureSourceKind, CaptureTarget, Codec, FrameRate, Quality, RecorderMetrics, RecorderSettings,
    Resolution, ScreenRecordingPermissionStatus,
};
use futures::{channel::mpsc::UnboundedSender, StreamExt};
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
    time::Duration,
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
    Started(std::result::Result<JobSnapshot, String>),
    JobPolled(std::result::Result<JobSnapshot, String>),
    Paused(std::result::Result<JobSnapshot, String>),
    Resumed(std::result::Result<JobSnapshot, String>),
    Stopped(std::result::Result<JobSnapshot, String>),
}

enum UiEvent {
    App(AppEvent),
}

pub(crate) struct WrecApp {
    daemon: DaemonClient,
    app_events: UnboundedSender<UiEvent>,
    pub(crate) settings: RecorderSettings,
    targets: Vec<CaptureTarget>,
    selected_target_key: Option<String>,
    active_job_id: Option<u64>,
    active_job_event_count: usize,
    active_output_path: Option<PathBuf>,
    pub(crate) recorder_state: RecorderState,
    pub(crate) permission_status: ScreenRecordingPermissionStatus,
    pub(crate) permission_busy: bool,
    pub(crate) metrics: Option<RecorderMetrics>,
    pub(crate) status: String,
    pub(crate) cli_install_status: CliInstallStatus,
    pub(crate) active_tab: AppTab,
    pub(crate) last_recording_dir: Option<PathBuf>,
    pub(crate) show_nerd_logs: bool,
    pub(crate) logs: VecDeque<String>,
    pub(crate) source_select: Entity<ControlSelect>,
    pub(crate) target_select: Entity<TargetSelect>,
    pub(crate) codec_select: Entity<ControlSelect>,
    pub(crate) quality_select: Entity<ControlSelect>,
    pub(crate) resolution_select: Entity<LimitedSelect>,
    pub(crate) fps_select: Entity<LimitedSelect>,
    pub(crate) output_input: Entity<InputState>,
    _event_task: Task<()>,
}

impl WrecApp {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = AppConfig::load();
        let settings = config.settings.with_preset_limits();

        let (ui_events, mut ui_receiver) = futures::channel::mpsc::unbounded();
        let app_events = ui_events.clone();
        let event_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = ui_receiver.next().await {
                if this
                    .update_in(cx, |this, window, cx| match event {
                        UiEvent::App(event) => {
                            this.handle_app_event(event, window, cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
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
        let resolution_select = cx.new(|cx| {
            SelectState::new(
                resolution_options_for(settings.quality),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let fps_select = cx.new(|cx| {
            SelectState::new(
                fps_options_for(settings.quality),
                Some(IndexPath::default()),
                window,
                cx,
            )
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
            daemon: DaemonClient::new(),
            app_events,
            settings,
            targets: Vec::new(),
            selected_target_key: config.selected_target_key,
            active_job_id: None,
            active_job_event_count: 0,
            active_output_path: None,
            recorder_state: RecorderState::Idle,
            permission_status: ScreenRecordingPermissionStatus::Unknown,
            permission_busy: false,
            metrics: None,
            status: "Idle".to_string(),
            cli_install_status: crate::platform::cli_install_status(),
            active_tab: AppTab::General,
            last_recording_dir: None,
            show_nerd_logs: config.show_nerd_logs,
            logs: VecDeque::new(),
            source_select,
            target_select,
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
        window: &mut Window,
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
        self.push_log(format!("preset: {value}"));
        self.apply_preset_limits(window, cx);
        self.save_config();
        cx.notify();
    }

    fn on_resolution_select(
        &mut self,
        _: &Entity<LimitedSelect>,
        event: &SelectEvent<Vec<LimitedOption>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        let resolution = resolution_from_label(value.as_ref());
        if resolution_disabled(self.settings.quality, resolution) {
            self.sync_capture_selects(window, cx);
            cx.notify();
            return;
        }
        self.settings.resolution = resolution;
        self.apply_preset_limits(window, cx);
        self.push_log(format!("resolution: {value}"));
        self.save_config();
        cx.notify();
    }

    fn on_fps_select(
        &mut self,
        _: &Entity<LimitedSelect>,
        event: &SelectEvent<Vec<LimitedOption>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        let fps = fps_from_label(value.as_ref());
        if fps_disabled(self.settings.quality, fps) {
            self.sync_capture_selects(window, cx);
            cx.notify();
            return;
        }
        self.set_fps(fps, window, cx);
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

    fn set_fps(&mut self, fps: FrameRate, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.fps = fps;
        self.apply_preset_limits(window, cx);
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
        // Don't auto-jump to the Nerd tab when enabling logs; only move away
        // from it when it's being hidden.
        if !show_nerd_logs && self.active_tab == AppTab::Nerd {
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

    pub(crate) fn copy_cli_install_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cli_install_status = crate::platform::cli_install_status();
        let Some(command) = crate::platform::cli_install_command() else {
            self.show_error(
                "Package Wrec as an app before installing the CLI",
                window,
                cx,
            );
            return;
        };

        cx.write_to_clipboard(ClipboardItem::new_string(command));
        self.push_log("copied CLI install command");
        push_app_notification(
            window,
            Notification::new().message("CLI install command copied"),
            cx,
        );
        cx.notify();
    }

    pub(crate) fn refresh_cli_install_status(&mut self, cx: &mut Context<Self>) {
        self.cli_install_status = crate::platform::cli_install_status();
        self.push_log(format!(
            "cli install status: {}",
            self.cli_install_status.label()
        ));
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
        let daemon = self.daemon.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = daemon
                .ensure()
                .and_then(|_| daemon.screen_recording_permission_status())
                .map_err(agent_error_message);
            let _ = app_events.unbounded_send(UiEvent::App(AppEvent::PermissionChecked {
                result,
                refresh_targets_after_granted,
            }));
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
        let daemon = self.daemon.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = daemon
                .ensure()
                .and_then(|_| daemon.request_screen_recording_permission())
                .map_err(agent_error_message);
            let _ = app_events.unbounded_send(UiEvent::App(AppEvent::PermissionRequested(result)));
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
        let daemon = self.daemon.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || {
            let result = daemon
                .ensure()
                .and_then(|_| daemon.list_targets())
                .map_err(agent_error_message);
            let _ = app_events.unbounded_send(UiEvent::App(AppEvent::TargetsLoaded(result)));
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

    fn sync_capture_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.codec_select.update(cx, |select, cx| {
            select.set_selected_value(&codec_label(self.settings.codec).into(), window, cx);
        });
        self.quality_select.update(cx, |select, cx| {
            select.set_selected_value(&quality_label(self.settings.quality).into(), window, cx);
        });
        self.resolution_select.update(cx, |select, cx| {
            select.set_items(resolution_options_for(self.settings.quality), window, cx);
            select.set_selected_value(
                &resolution_label(self.settings.resolution).into(),
                window,
                cx,
            );
        });
        self.fps_select.update(cx, |select, cx| {
            select.set_items(fps_options_for(self.settings.quality), window, cx);
            select.set_selected_value(&fps_label(self.settings.fps).into(), window, cx);
        });
    }

    fn apply_preset_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = (self.settings.resolution, self.settings.fps);
        self.settings = self.settings.clone().with_preset_limits();
        let after = (self.settings.resolution, self.settings.fps);

        if before != after {
            self.push_log(format!(
                "preset limit: {} maxes at {}, {} fps",
                quality_label(self.settings.quality),
                resolution_label(self.settings.resolution),
                self.settings.fps.as_u32()
            ));
        }
        self.sync_capture_selects(window, cx);
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
            let Some(job_id) = self.active_job_id else {
                self.show_error("No active daemon job to stop", window, cx);
                return;
            };
            self.recorder_state = RecorderState::Stopping;
            self.status = "Stopping".to_string();
            self.push_log("stopping recording");
            let daemon = self.daemon.clone();
            let app_events = self.app_events.clone();
            std::thread::spawn(move || {
                let result = daemon.stop_job(job_id).map_err(agent_error_message);
                let _ = app_events.unbounded_send(UiEvent::App(AppEvent::Stopped(result)));
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
        self.active_job_id = None;
        self.active_job_event_count = 0;
        self.active_output_path = None;
        let daemon = self.daemon.clone();
        let app_events = self.app_events.clone();
        let settings = self.settings.clone();
        std::thread::spawn(move || {
            let result = daemon
                .ensure()
                .and_then(|_| daemon.start_recording(recording_params(target, settings)))
                .map_err(agent_error_message);
            let _ = app_events.unbounded_send(UiEvent::App(AppEvent::Started(result)));
        });
        cx.notify();
    }

    pub(crate) fn toggle_pause(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.recorder_state {
            RecorderState::Recording => {
                let Some(job_id) = self.active_job_id else {
                    self.show_error("No active daemon job to pause", window, cx);
                    return;
                };
                self.recorder_state = RecorderState::Pausing;
                self.status = "Pausing".to_string();
                self.push_log("pausing recording");
                let daemon = self.daemon.clone();
                let app_events = self.app_events.clone();
                std::thread::spawn(move || {
                    let result = daemon.pause_job(job_id).map_err(agent_error_message);
                    let _ = app_events.unbounded_send(UiEvent::App(AppEvent::Paused(result)));
                });
                cx.notify();
            }
            RecorderState::Paused => {
                let Some(job_id) = self.active_job_id else {
                    self.show_error("No active daemon job to resume", window, cx);
                    return;
                };
                self.recorder_state = RecorderState::Resuming;
                self.status = "Resuming".to_string();
                self.push_log("resuming recording");
                let daemon = self.daemon.clone();
                let app_events = self.app_events.clone();
                std::thread::spawn(move || {
                    let result = daemon.resume_job(job_id).map_err(agent_error_message);
                    let _ = app_events.unbounded_send(UiEvent::App(AppEvent::Resumed(result)));
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
            AppEvent::Started(Ok(job)) => {
                if !matches!(self.recorder_state, RecorderState::Starting) {
                    self.push_log(format!("ignored late start for job {}", job.id));
                    cx.notify();
                    return;
                }
                self.active_job_id = Some(job.id);
                self.active_job_event_count = 0;
                self.apply_job_snapshot(job, window, cx);
                self.start_job_poll();
                push_app_notification(
                    window,
                    Notification::new().message("Recording submitted"),
                    cx,
                );
            }
            AppEvent::Started(Err(message)) => {
                if !matches!(self.recorder_state, RecorderState::Starting) {
                    self.push_log(format!("ignored late start failure: {message}"));
                    cx.notify();
                    return;
                }
                self.active_job_id = None;
                self.active_job_event_count = 0;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                if is_permission_message(&message) {
                    self.permission_status = ScreenRecordingPermissionStatus::Missing;
                }
                cx.activate(true);
                window.activate_window();
                self.show_error(message, window, cx);
            }
            AppEvent::JobPolled(Ok(job)) => {
                if self.should_accept_job(job.id) {
                    self.apply_job_snapshot(job, window, cx);
                }
            }
            AppEvent::JobPolled(Err(message)) => {
                if self.active_job_id.is_some() {
                    self.recorder_state = RecorderState::Failed;
                    self.active_job_id = None;
                    self.active_output_path = None;
                    self.show_error(message, window, cx);
                }
            }
            AppEvent::Paused(Ok(job)) => {
                if !matches!(self.recorder_state, RecorderState::Pausing) {
                    cx.notify();
                    return;
                }
                self.apply_job_snapshot(job, window, cx);
            }
            AppEvent::Paused(Err(message)) => {
                if matches!(self.recorder_state, RecorderState::Pausing) {
                    self.recorder_state = RecorderState::Recording;
                }
                self.show_error(message, window, cx);
            }
            AppEvent::Resumed(Ok(job)) => {
                if !matches!(self.recorder_state, RecorderState::Resuming) {
                    cx.notify();
                    return;
                }
                self.apply_job_snapshot(job, window, cx);
            }
            AppEvent::Resumed(Err(message)) => {
                if matches!(self.recorder_state, RecorderState::Resuming) {
                    self.recorder_state = RecorderState::Paused;
                }
                self.show_error(message, window, cx);
            }
            AppEvent::Stopped(Ok(job)) => {
                self.apply_job_snapshot(job, window, cx);
            }
            AppEvent::Stopped(Err(message)) => {
                self.active_job_id = None;
                self.active_job_event_count = 0;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                cx.activate(true);
                window.activate_window();
                self.show_error(message, window, cx);
            }
        }
    }

    fn apply_job_snapshot(
        &mut self,
        job: JobSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.should_accept_job(job.id) {
            return;
        }

        let was_stopping = matches!(self.recorder_state, RecorderState::Stopping);
        self.active_job_id.get_or_insert(job.id);
        if let Some(path) = job.output_path.clone() {
            self.active_output_path = Some(path.clone());
            self.last_recording_dir = path.parent().map(Path::to_path_buf);
        }
        self.push_new_job_events(&job);
        if let Some(metrics) = job
            .events
            .iter()
            .rev()
            .find_map(|event| event.metrics.clone())
        {
            self.metrics = Some(metrics);
        }

        match job.status {
            JobStatus::Queued | JobStatus::Starting => {
                self.recorder_state = RecorderState::Starting;
                self.status = job
                    .target
                    .as_ref()
                    .map(|target| format!("Starting {}", target.name))
                    .unwrap_or_else(|| "Starting".to_string());
            }
            JobStatus::Recording => {
                self.recorder_state = RecorderState::Recording;
                self.status = job
                    .output_path
                    .as_ref()
                    .map(|path| format!("Recording to {}", path.display()))
                    .unwrap_or_else(|| "Recording".to_string());
            }
            JobStatus::Paused => {
                self.recorder_state = RecorderState::Paused;
                self.status = "Paused".to_string();
            }
            JobStatus::Finishing => {
                self.recorder_state = RecorderState::Stopping;
                self.status = "Stopping".to_string();
            }
            JobStatus::Completed => {
                self.active_job_id = None;
                self.active_job_event_count = 0;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Idle;
                self.status = job
                    .output_path
                    .as_ref()
                    .map(|path| format!("Saved to {}", path.display()))
                    .unwrap_or_else(|| "Recording completed".to_string());
                cx.activate(true);
                window.activate_window();
                if was_stopping {
                    self.open_last_recording_folder(window, cx);
                    push_app_notification(
                        window,
                        Notification::new().message("Recording stopped"),
                        cx,
                    );
                }
            }
            JobStatus::Failed | JobStatus::Cancelled => {
                let message = latest_error_message(&job).unwrap_or_else(|| {
                    if matches!(job.status, JobStatus::Cancelled) {
                        "Recording cancelled".to_string()
                    } else {
                        "Recording failed".to_string()
                    }
                });
                self.active_job_id = None;
                self.active_job_event_count = 0;
                self.active_output_path = None;
                self.recorder_state = RecorderState::Failed;
                self.status = message.clone();
                if is_permission_message(&message) {
                    self.permission_status = ScreenRecordingPermissionStatus::Missing;
                }
                cx.activate(true);
                window.activate_window();
                self.show_error(message, window, cx);
            }
        }
        cx.notify();
    }

    fn push_new_job_events(&mut self, job: &JobSnapshot) {
        for event in job.events.iter().skip(self.active_job_event_count) {
            self.push_log_entry(event.message.clone());
        }
        self.active_job_event_count = job.events.len();
    }

    fn should_accept_job(&self, job_id: u64) -> bool {
        self.active_job_id
            .map(|active_job_id| active_job_id == job_id)
            .unwrap_or(false)
    }

    fn start_job_poll(&self) {
        let Some(job_id) = self.active_job_id else {
            return;
        };
        let daemon = self.daemon.clone();
        let app_events = self.app_events.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let result = daemon.show_job(job_id).map_err(agent_error_message);
            let stop = result.as_ref().map_or(true, |job| is_terminal_job(job));
            let _ = app_events.unbounded_send(UiEvent::App(AppEvent::JobPolled(result)));
            if stop {
                break;
            }
        });
    }

    fn open_last_recording_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    }

    fn show_error(
        &mut self,
        message: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = message.into();
        self.status = message.clone();
        self.push_log_entry(format!("error: {message}"));
        tracing::error!("{message}");
        push_app_notification(
            window,
            Notification::new().message(message).autohide(false),
            cx,
        );
    }

    pub(crate) fn push_log(&mut self, message: impl Into<String>) {
        self.push_log_entry(message);
    }

    fn push_log_entry(&mut self, message: impl Into<String>) {
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
            show_nerd_logs: self.show_nerd_logs,
        };

        if let Err(err) = persist_config(&config) {
            self.push_log(format!("config save failed: {err}"));
            tracing::warn!("failed to save config: {err}");
        }
    }
}

fn recording_params(target: CaptureTarget, settings: RecorderSettings) -> StartRecordingParams {
    StartRecordingParams {
        selector: Some(TargetSelector::Id {
            kind: target.kind,
            id: target.id,
        }),
        options: RecordingOptions {
            source_kind: Some(settings.source),
            fps: Some(settings.fps),
            codec: Some(settings.codec),
            quality: Some(settings.quality),
            resolution: Some(settings.resolution),
            output_dir: Some(settings.output_dir),
            include_cursor: Some(settings.include_cursor),
            include_system_audio: Some(settings.include_system_audio),
            hide_wrec: Some(settings.hide_wrec),
        },
        duration_ms: None,
        queue: false,
    }
}

fn agent_error_message(error: AgentError) -> String {
    format!("{} Next: {}", error.message, error.next)
}

fn is_terminal_job(job: &JobSnapshot) -> bool {
    matches!(
        job.status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
    )
}

fn latest_error_message(job: &JobSnapshot) -> Option<String> {
    job.events
        .iter()
        .rev()
        .find(|event| matches!(event.level, EventLevel::Error))
        .map(|event| event.message.clone())
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

fn resolution_from_label(label: &str) -> Resolution {
    match label {
        "720p" => Resolution::R720p,
        "1080p" => Resolution::R1080p,
        "2K" => Resolution::R2k,
        "4K" => Resolution::R4k,
        _ => Resolution::Native,
    }
}

fn fps_from_label(label: &str) -> FrameRate {
    match label {
        "60 FPS" => FrameRate::Fps60,
        _ => FrameRate::Fps30,
    }
}

fn is_permission_message(message: &str) -> bool {
    message.contains("Screen Recording") || message.contains("screen recording permission")
}

#[cfg(test)]
mod tests {
    use super::recording_params;
    use domain::{CaptureSourceKind, CaptureTarget, RecorderSettings};

    #[test]
    fn app_recordings_do_not_queue_without_queue_ui() {
        let target = CaptureTarget {
            id: 7,
            name: "Main Display".into(),
            kind: CaptureSourceKind::Display,
        };
        let params = recording_params(target, RecorderSettings::default());

        assert!(!params.queue);
    }
}
