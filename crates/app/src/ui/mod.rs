pub(crate) mod kit;

use crate::{
    app::WrecApp,
    assets::{PhosphorIcon, GEIST_FONT_FAMILY, GEIST_MONO_FONT_FAMILY},
    platform::{CliInstallStatus, SkillInstallStatus},
};
use domain::{
    CaptureSourceKind, CaptureTarget, FrameRate, PermissionStatus, Quality, RecorderMetrics,
    Resolution,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    input::Input, notification::Notification, Root, Sizable as _, Theme, ThemeMode, WindowExt as _,
};
use kit::{kit_icon, KitButton, KitSwitch, Picker, PickerItem, PickerState, Tokens, RADIUS};
use std::rc::Rc;

pub(crate) type ControlSelect = PickerState<&'static str>;
pub(crate) type LimitedSelect = PickerState<LimitedOption>;
pub(crate) type TargetSelect = PickerState<TargetOption>;

pub(crate) const CONTROL_HEIGHT: f32 = 32.;
const RECORD_BUTTON_HEIGHT: f32 = 48.;
pub(crate) const WINDOW_WIDTH: f32 = 628.;
pub(crate) const WINDOW_HEIGHT: f32 = 540.;
pub(crate) const WINDOW_MIN_WIDTH: f32 = 608.;
pub(crate) const WINDOW_MIN_HEIGHT: f32 = 500.;
pub(crate) const SOURCE_OPTIONS: [&str; 2] = ["Display", "Window"];
pub(crate) const CODEC_OPTIONS: [&str; 2] = ["HEVC", "H.264"];
pub(crate) const QUALITY_OPTIONS: [&str; 3] = ["Balanced", "Efficient", "High"];

const SIDEBAR_WIDTH: f32 = 154.;
const TITLE_BAR_HEIGHT: f32 = 40.;
const NATIVE_WINDOW_CONTROLS_WIDTH: f32 = 72.;
const FIELD_LABEL_WIDTH: f32 = 96.;
const NOTIFICATION_WIDTH: f32 = 320.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppTab {
    General,
    Settings,
    Cli,
    About,
    Nerd,
}

impl AppTab {
    fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Settings => "settings",
            Self::Cli => "cli",
            Self::About => "about",
            Self::Nerd => "nerd",
        }
    }

    fn is_active(self, active_tab: Self) -> bool {
        self == active_tab
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetOption {
    key: SharedString,
    title: SharedString,
}

impl TargetOption {
    pub(crate) fn new(target: &CaptureTarget) -> Self {
        Self {
            key: target_key(target).into(),
            title: target.name.clone().into(),
        }
    }

    pub(crate) fn key(&self) -> &SharedString {
        &self.key
    }
}

impl PickerItem for TargetOption {
    fn label(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> SharedString {
        self.key.clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LimitedOption {
    value: SharedString,
    title: SharedString,
    disabled: bool,
}

impl LimitedOption {
    fn new(label: &'static str, disabled: bool) -> Self {
        Self {
            value: label.into(),
            title: label.into(),
            disabled,
        }
    }
}

impl PickerItem for LimitedOption {
    fn label(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> SharedString {
        self.value.clone()
    }

    fn disabled(&self) -> bool {
        self.disabled
    }
}

pub(crate) fn resolution_options_for(quality: Quality) -> Vec<LimitedOption> {
    [
        (Resolution::Native, "Original"),
        (Resolution::R4k, "4K"),
        (Resolution::R2k, "2K"),
        (Resolution::R1080p, "1080p"),
        (Resolution::R720p, "720p"),
    ]
    .into_iter()
    .map(|(resolution, label)| LimitedOption::new(label, resolution_disabled(quality, resolution)))
    .collect()
}

pub(crate) fn fps_options_for(quality: Quality) -> Vec<LimitedOption> {
    [(FrameRate::Fps30, "30 FPS"), (FrameRate::Fps60, "60 FPS")]
        .into_iter()
        .map(|(fps, label)| LimitedOption::new(label, fps_disabled(quality, fps)))
        .collect()
}

pub(crate) fn resolution_disabled(quality: Quality, resolution: Resolution) -> bool {
    quality
        .max_resolution()
        .is_some_and(|cap| resolution.capped_at(cap) != resolution)
}

pub(crate) fn fps_disabled(quality: Quality, fps: FrameRate) -> bool {
    fps.capped_at(quality.max_fps()) != fps
}

impl WrecApp {
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let live = self.recorder_state.is_active_session();
        let paused = self.recorder_state.is_paused();
        let elapsed = self
            .metrics
            .as_ref()
            .map(|metrics| metrics.elapsed_secs)
            .unwrap_or(0);

        div()
            .id("wrec-titlebar")
            .flex()
            .items_center()
            .justify_between()
            .h(px(TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .pl(px(14.))
            .pr_2()
            .border_b_1()
            .border_color(t.line)
            .child(
                div()
                    .w(px(NATIVE_WINDOW_CONTROLS_WIDTH))
                    .h_full()
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .when(live, |this| {
                let (dot, text_color, label) = if paused {
                    (t.ink_muted, t.ink_muted, "PAUSED".to_string())
                } else {
                    (
                        t.accent,
                        t.accent,
                        format!("REC {}:{:02}", elapsed / 60, elapsed % 60),
                    )
                };
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .mr_3()
                        .child(div().size(px(6.)).rounded_full().bg(dot))
                        .child(
                            div()
                                .font_family(GEIST_MONO_FONT_FAMILY)
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(text_color)
                                .child(label),
                        ),
                )
            })
            .child(theme_toggle(t.is_dark, cx))
    }

    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let active = self.active_tab;
        let items = [
            Some(sidebar_nav_item(
                "General",
                PhosphorIcon::Gauge,
                AppTab::General,
                AppTab::General.is_active(active),
                cx,
            )),
            Some(sidebar_nav_item(
                "CLI",
                PhosphorIcon::Terminal,
                AppTab::Cli,
                AppTab::Cli.is_active(active),
                cx,
            )),
            self.show_nerd_logs.then(|| {
                sidebar_nav_item(
                    "Nerd",
                    PhosphorIcon::Pulse,
                    AppTab::Nerd,
                    AppTab::Nerd.is_active(active),
                    cx,
                )
            }),
            Some(sidebar_nav_item(
                "Settings",
                PhosphorIcon::Gear,
                AppTab::Settings,
                AppTab::Settings.is_active(active),
                cx,
            )),
            Some(sidebar_nav_item(
                "About",
                PhosphorIcon::Info,
                AppTab::About,
                AppTab::About.is_active(active),
                cx,
            )),
        ]
        .into_iter()
        .flatten()
        .collect();

        div()
            .id("wrec-sidebar")
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .pt_3()
            .bg(t.panel)
            .text_color(t.ink)
            .border_r_1()
            .border_color(t.line)
            .child(WrecSidebarNav { items }.render("wrec-sidebar-nav", cx))
    }

    pub(crate) fn render_general_tab(
        &self,
        record_icon: PhosphorIcon,
        record_label: &'static str,
        record_tip: &'static str,
        record_is_idle: bool,
        record_disabled: bool,
        show_pause_button: bool,
        pause_icon: PhosphorIcon,
        pause_label: &'static str,
        pause_tip: &'static str,
        pause_disabled: bool,
        controls_disabled: bool,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let source_row = labeled_control_row(
            "Source",
            muted_foreground,
            Picker::new("source-picker", &self.source_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Source")
                .menu_max_h(112.)
                .disabled(controls_disabled),
        );
        let target_row = labeled_control_row(
            "Target",
            muted_foreground,
            Picker::new("target-picker", &self.target_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Target")
                .menu_max_h(224.)
                .disabled(controls_disabled),
        );
        let format_row = labeled_control_row(
            "Format",
            muted_foreground,
            Picker::new("codec-picker", &self.codec_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Format")
                .disabled(controls_disabled),
        );
        let quality_row = labeled_control_row(
            "Preset",
            muted_foreground,
            Picker::new("quality-picker", &self.quality_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Preset")
                .disabled(controls_disabled),
        );
        let resolution_row = labeled_control_row(
            "Resolution",
            muted_foreground,
            Picker::new("resolution-picker", &self.resolution_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Resolution")
                .disabled(controls_disabled),
        );
        let frame_rate_row = labeled_control_row(
            "Frame Rate",
            muted_foreground,
            Picker::new("fps-picker", &self.fps_select)
                .height(CONTROL_HEIGHT)
                .placeholder("Frame Rate")
                .disabled(controls_disabled),
        );
        let cursor_row = label_switch_row(
            "Cursor",
            muted_foreground,
            KitSwitch::new("cursor-switch")
                .checked(self.settings.include_cursor)
                .tooltip("Capture cursor")
                .disabled(controls_disabled)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.set_include_cursor(*checked, cx);
                })),
        );
        let audio_row = label_switch_row(
            "System Audio",
            muted_foreground,
            KitSwitch::new("system-audio-switch")
                .checked(self.settings.include_system_audio)
                .tooltip("Capture system audio")
                .disabled(controls_disabled)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.set_include_system_audio(*checked, cx);
                })),
        );
        let mic_permission_missing =
            self.settings.include_microphone && !self.mic_permission_status.is_granted();
        let microphone_row = div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(CONTROL_HEIGHT))
            .gap_3()
            .child(field_label("Mic", muted_foreground))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(mic_permission_missing, |this| {
                        this.child(mic_permission_button(
                            "microphone-permission-grant",
                            self.mic_permission_status,
                            self.mic_permission_busy,
                            cx,
                        ))
                    })
                    .child(
                        KitSwitch::new("microphone-switch")
                            .checked(self.settings.include_microphone)
                            .tooltip("Capture microphone")
                            .disabled(controls_disabled)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.set_include_microphone(*checked, cx);
                            })),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(source_row)
                    .child(target_row),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(format_row)
                    .child(resolution_row)
                    .child(quality_row)
                    .child(frame_rate_row)
                    .child(cursor_row)
                    .child(audio_row)
                    .child(microphone_row),
            )
            .child(if show_pause_button {
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(pause_button(
                        pause_icon,
                        pause_label,
                        pause_tip,
                        pause_disabled,
                        cx,
                    )))
                    .child(div().flex_1().child(record_button(
                        record_icon,
                        record_label,
                        record_tip,
                        record_is_idle,
                        record_disabled,
                        cx,
                    )))
                    .into_any_element()
            } else {
                record_button(
                    record_icon,
                    record_label,
                    record_tip,
                    record_is_idle,
                    record_disabled,
                    cx,
                )
                .w_full()
                .into_any_element()
            })
    }

    pub(crate) fn render_settings_tab(
        &self,
        controls_disabled: bool,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Screen Recording Access"))
                    .child(permission_state_button(
                        self.permission_status,
                        self.permission_busy,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Microphone Access"))
                    .child(mic_permission_button(
                        "settings-microphone-state",
                        self.mic_permission_status,
                        self.mic_permission_busy,
                        cx,
                    )),
            )
            .child(label_switch_row(
                "Hide wrec",
                muted_foreground,
                KitSwitch::new("hide-window-switch")
                    .checked(self.settings.hide_wrec)
                    .tooltip("Hide wrec from recording")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.set_hide_wrec(*checked, cx);
                    })),
            ))
            .child(label_switch_row(
                "Logs",
                muted_foreground,
                KitSwitch::new("logs-switch")
                    .checked(self.show_nerd_logs)
                    .tooltip("Show Nerd tab")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.set_show_nerd_logs(*checked, cx);
                    })),
            ))
            .child(
                div().w_full().h(px(CONTROL_HEIGHT)).child(
                    Input::new(&self.output_input)
                        .large()
                        .h(px(CONTROL_HEIGHT))
                        .disabled(controls_disabled),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_3()
                    .child(
                        div().flex_1().child(
                            KitButton::new("choose-output-dir")
                                .w_full()
                                .height(CONTROL_HEIGHT)
                                .icon(PhosphorIcon::FolderOpen)
                                .icon_color(muted_foreground)
                                .label("Choose")
                                .tooltip("Choose output folder")
                                .disabled(controls_disabled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_output_dir(window, cx);
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            KitButton::new("open-last-recording-dir")
                                .w_full()
                                .height(CONTROL_HEIGHT)
                                .icon(PhosphorIcon::FolderOpen)
                                .icon_color(muted_foreground)
                                .label("Open")
                                .tooltip("Open last recording folder")
                                .disabled(self.last_recording_dir.is_none())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_last_recording_dir(window, cx);
                                })),
                        ),
                    ),
            )
    }

    pub(crate) fn render_cli_tab(
        &self,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cli_command = crate::platform::cli_install_command();
        let cli_installed = self.cli_install_status == CliInstallStatus::Installed;
        let cli_button_label = match self.cli_install_status {
            CliInstallStatus::Installed => "Installed",
            CliInstallStatus::NeedsUpdate => "Update",
            CliInstallStatus::NotInstalled | CliInstallStatus::Conflict => "Copy",
        };
        let cli_tooltip = if cli_installed {
            "CLI is installed"
        } else {
            "Copy CLI install command"
        };
        let skill_installed = self.skill_install_status == SkillInstallStatus::Installed;
        let skill_button_label = match self.skill_install_status {
            SkillInstallStatus::Installed => "Installed",
            SkillInstallStatus::NeedsUpdate => "Update",
            SkillInstallStatus::NotInstalled => "Install",
        };
        let skill_tooltip = if skill_installed {
            "Skill is installed"
        } else {
            "Install the wrec skill to ~/.claude/skills/wrec"
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("CLI"))
                    .child(
                        KitButton::new("cli-copy-install")
                            .height(CONTROL_HEIGHT)
                            .icon(PhosphorIcon::Clipboard)
                            .icon_color(muted_foreground)
                            .label(cli_button_label)
                            .tooltip(cli_tooltip)
                            .disabled(cli_installed || cli_command.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.copy_cli_install_command(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Skill"))
                    .child(
                        KitButton::new("skill-install")
                            .height(CONTROL_HEIGHT)
                            .icon(PhosphorIcon::Download)
                            .icon_color(muted_foreground)
                            .label(skill_button_label)
                            .tooltip(skill_tooltip)
                            .disabled(skill_installed)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.install_wrec_skill(window, cx);
                            })),
                    ),
            )
    }

    pub(crate) fn render_nerds_tab(
        &self,
        metrics_label: Option<String>,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let metrics_label = metrics_label.unwrap_or_else(zero_metrics_label);

        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Logs"))
                    .child(
                        KitButton::new("open-recordings-data-dir")
                            .height(CONTROL_HEIGHT)
                            .icon(PhosphorIcon::FolderOpen)
                            .icon_color(muted_foreground)
                            .label("Open")
                            .tooltip("Open recordings data folder")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_recordings_data_dir(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .px_3()
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .text_center()
                            .text_size(px(24.))
                            .line_height(relative(1.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_family(GEIST_MONO_FONT_FAMILY)
                            .child(metrics_label),
                    ),
            )
    }

    pub(crate) fn render_about_tab(
        &self,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let update_eligible = crate::updater::eligible_bundle().is_ok();
        let (update_label, update_disabled, update_tooltip): (String, bool, String) =
            if !update_eligible {
                (
                    "Unavailable".into(),
                    true,
                    "Dev and source builds update by rebuilding".into(),
                )
            } else {
                use crate::updater::AppUpdateState;
                match &self.app_update {
                    AppUpdateState::Idle => (
                        "Check for updates".into(),
                        false,
                        "Check GitHub for a newer release".into(),
                    ),
                    AppUpdateState::Checking => ("Checking…".into(), true, "Checking…".into()),
                    AppUpdateState::UpToDate => (
                        "Up to date".into(),
                        true,
                        "You are on the latest release".into(),
                    ),
                    AppUpdateState::Available { version } => (
                        format!("Update to {version}"),
                        false,
                        "Download, verify, and relaunch into the new version".into(),
                    ),
                    AppUpdateState::Updating { .. } => {
                        ("Updating…".into(), true, "Updating…".into())
                    }
                    AppUpdateState::Failed { message } => {
                        ("Check again".into(), false, message.clone())
                    }
                }
            };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(plain_info_row(
                "Version",
                env!("CARGO_PKG_VERSION"),
                muted_foreground,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Updates"))
                    .child(
                        KitButton::new("app-update")
                            .height(CONTROL_HEIGHT)
                            .icon(PhosphorIcon::Download)
                            .icon_color(muted_foreground)
                            .label(update_label)
                            .tooltip(update_tooltip)
                            .disabled(update_disabled)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                use crate::updater::AppUpdateState;
                                match this.app_update {
                                    AppUpdateState::Available { .. } => this.install_app_update(cx),
                                    _ => this.check_for_app_update(cx),
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                KitButton::new("open-github")
                    .w_full()
                    .height(CONTROL_HEIGHT)
                    .icon(PhosphorIcon::Github)
                    .icon_color(muted_foreground)
                    .label("GitHub")
                    .tooltip("Open GitHub repository")
                    .on_click(cx.listener(|this, _, window, cx| {
                        match crate::platform::open_url(crate::app::GITHUB_URL) {
                            Ok(()) => this.push_log("opened GitHub repository"),
                            Err(err) => {
                                this.push_log(format!("open GitHub failed: {err}"));
                                push_app_notification(
                                    window,
                                    app_notification(format!(
                                        "Could not open GitHub repository: {err}"
                                    )),
                                    cx,
                                );
                            }
                        }
                        cx.notify();
                    })),
            )
    }
}

impl Render for WrecApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let muted_foreground = t.ink_muted;
        let notification_layer = Root::render_notification_layer(window, cx);
        let active_session = self.recorder_state.is_active_session();
        let (record_icon, record_label, record_tip, record_is_idle) = if active_session {
            (PhosphorIcon::Stop, "Stop", "Stop recording", false)
        } else {
            (PhosphorIcon::FilmReel, "Record", "Start recording", true)
        };
        let (pause_icon, pause_label, pause_tip) = if self.recorder_state.is_paused() {
            (PhosphorIcon::Play, "Resume", "Resume recording")
        } else {
            (PhosphorIcon::Pause, "Pause", "Pause recording")
        };
        let record_disabled = matches!(
            self.recorder_state,
            crate::app::RecorderState::Starting
                | crate::app::RecorderState::Pausing
                | crate::app::RecorderState::Resuming
                | crate::app::RecorderState::Stopping
        ) || (!active_session
            && (self.permission_busy || !self.permission_status.is_granted()));
        let pause_disabled = matches!(
            self.recorder_state,
            crate::app::RecorderState::Pausing
                | crate::app::RecorderState::Resuming
                | crate::app::RecorderState::Stopping
        );
        let controls_disabled =
            self.recorder_state.is_busy() || self.permission_busy || active_session;
        let metrics_label = Some(if active_session || self.recorder_state.is_recording() {
            self.metrics
                .as_ref()
                .map(metrics_label)
                .unwrap_or_else(zero_metrics_label)
        } else {
            zero_metrics_label()
        });

        div()
            .id("wrec-root")
            .on_action(cx.listener(WrecApp::on_minimize_action))
            .on_action(cx.listener(WrecApp::on_hide_action))
            .on_action(cx.listener(WrecApp::on_quit_action))
            .relative()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(t.line)
            .bg(t.surface)
            .text_color(t.ink)
            .font_family(GEIST_FONT_FAMILY)
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .flex()
            .flex_col()
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.render_sidebar(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .pt_4()
                            .pb_4()
                            .pl_4()
                            .pr_3()
                            .child(div().id("tab-content").flex().flex_col().flex_1().map(
                                |this| match self.active_tab {
                                    AppTab::General => this.child(self.render_general_tab(
                                        record_icon,
                                        record_label,
                                        record_tip,
                                        record_is_idle,
                                        record_disabled,
                                        active_session,
                                        pause_icon,
                                        pause_label,
                                        pause_tip,
                                        pause_disabled,
                                        controls_disabled,
                                        muted_foreground,
                                        cx,
                                    )),
                                    AppTab::Settings => this.child(self.render_settings_tab(
                                        controls_disabled,
                                        muted_foreground,
                                        cx,
                                    )),
                                    AppTab::Cli => {
                                        this.child(self.render_cli_tab(muted_foreground, cx))
                                    }
                                    AppTab::Nerd if self.show_nerd_logs => this.child(
                                        self.render_nerds_tab(metrics_label, muted_foreground, cx),
                                    ),
                                    AppTab::Nerd => this.child(self.render_settings_tab(
                                        controls_disabled,
                                        muted_foreground,
                                        cx,
                                    )),
                                    AppTab::About => {
                                        this.child(self.render_about_tab(muted_foreground, cx))
                                    }
                                },
                            )),
                    ),
            )
            .children(notification_layer)
    }
}

type SidebarClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Clone)]
struct WrecSidebarNav {
    items: Vec<WrecSidebarNavItem>,
}

#[derive(Clone)]
struct WrecSidebarNavItem {
    label: &'static str,
    icon: PhosphorIcon,
    tab: AppTab,
    active: bool,
    on_click: SidebarClickHandler,
}

impl WrecSidebarNav {
    fn render(self, id: impl Into<ElementId>, cx: &mut Context<WrecApp>) -> impl IntoElement {
        let t = Tokens::get(cx);
        div()
            .id(id.into())
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .text_color(t.ink)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.items.into_iter().map(|item| sidebar_nav_row(item, cx))),
            )
    }
}

fn sidebar_nav_item(
    label: &'static str,
    icon: PhosphorIcon,
    tab: AppTab,
    active: bool,
    cx: &mut Context<WrecApp>,
) -> WrecSidebarNavItem {
    let on_click = Rc::new(cx.listener(move |this, _, _, cx| {
        if this.active_tab != tab {
            this.active_tab = tab;
            if tab == AppTab::Cli {
                this.refresh_cli_install_status(cx);
                this.refresh_skill_install_status(cx);
            }
            cx.notify();
        }
    }));

    WrecSidebarNavItem {
        label,
        icon,
        tab,
        active,
        on_click,
    }
}

fn sidebar_nav_row(item: WrecSidebarNavItem, cx: &mut Context<WrecApp>) -> impl IntoElement {
    let t = Tokens::get(cx);
    let on_click = item.on_click.clone();

    div()
        .id(format!("sidebar-nav-{}", item.tab.id()))
        .flex()
        .items_center()
        .w_full()
        .px_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .w_full()
                .h(px(30.))
                .px_2()
                .rounded(px(RADIUS))
                .cursor_pointer()
                .when(item.active, |this| this.bg(t.control))
                .when(!item.active, |this| {
                    this.hover(|this| this.bg(t.control.opacity(0.55)))
                })
                // Live-state bar: 2px of red marks the active tab.
                .child(
                    div()
                        .flex_none()
                        .w(px(2.))
                        .h(px(12.))
                        .when(item.active, |this| this.bg(t.accent)),
                )
                .child(kit_icon(
                    item.icon,
                    14.,
                    if item.active { t.ink } else { t.ink_muted },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .font_family(GEIST_MONO_FONT_FAMILY)
                        .text_size(px(12.))
                        .font_weight(if item.active {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(if item.active { t.ink } else { t.ink_muted })
                        .child(item.label),
                ),
        )
        .on_click(move |event, window, cx| {
            on_click(event, window, cx);
        })
}

fn permission_state_button(
    status: PermissionStatus,
    busy: bool,
    cx: &mut Context<WrecApp>,
) -> KitButton {
    let label = if busy {
        "Checking"
    } else if status.is_granted() {
        "Granted"
    } else {
        "Grant"
    };
    let tooltip = if status.is_granted() {
        "Screen Recording permission granted"
    } else {
        "Grant Screen Recording permission"
    };
    let button = KitButton::new("settings-screen-recording-state")
        .height(CONTROL_HEIGHT)
        .label(label)
        .tooltip(tooltip)
        .disabled(busy || status.is_granted())
        .on_click(cx.listener(|this, _, _, cx| {
            this.request_screen_recording_permission(cx);
        }));

    if !busy && !status.is_granted() {
        button.solid()
    } else {
        button
    }
}

fn mic_permission_button(
    id: &'static str,
    status: PermissionStatus,
    busy: bool,
    cx: &mut Context<WrecApp>,
) -> KitButton {
    let label = if busy {
        "Checking"
    } else if status.is_granted() {
        "Granted"
    } else {
        "Grant"
    };
    let tooltip = if status.is_granted() {
        "Microphone permission granted"
    } else {
        "Grant Microphone access. If macOS does not prompt, enable Wrec in System Settings > Privacy & Security > Microphone"
    };
    let button = KitButton::new(id)
        .height(CONTROL_HEIGHT)
        .label(label)
        .tooltip(tooltip)
        .disabled(busy || status.is_granted())
        .on_click(cx.listener(|this, _, _, cx| {
            this.request_microphone_permission(cx);
        }));

    if !busy && !status.is_granted() {
        button.solid()
    } else {
        button
    }
}

fn record_button(
    icon: PhosphorIcon,
    label: &'static str,
    tooltip: &'static str,
    _is_idle: bool,
    disabled: bool,
    cx: &mut Context<WrecApp>,
) -> KitButton {
    // Record and stop both wear the wrec red: the accent is reserved for
    // record/live states, and this is the live control.
    KitButton::new("record-button")
        .accent()
        .height(RECORD_BUTTON_HEIGHT)
        .text_size(13.)
        .icon(icon)
        .icon_size(16.)
        .label(label)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_recording(window, cx);
            cx.notify();
        }))
}

fn pause_button(
    icon: PhosphorIcon,
    label: &'static str,
    tooltip: &'static str,
    disabled: bool,
    cx: &mut Context<WrecApp>,
) -> KitButton {
    KitButton::new("pause-button")
        .w_full()
        .height(RECORD_BUTTON_HEIGHT)
        .text_size(13.)
        .icon(icon)
        .icon_size(16.)
        .label(label)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_pause(window, cx);
            cx.notify();
        }))
}

fn field_label(label: &'static str, color: Hsla) -> Div {
    div().w(px(FIELD_LABEL_WIDTH)).flex_none().child(
        div()
            .font_family(GEIST_MONO_FONT_FAMILY)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .child(label),
    )
}

fn row_label(label: &'static str) -> Div {
    div()
        .font_family(GEIST_MONO_FONT_FAMILY)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn labeled_control_row(label: &'static str, color: Hsla, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w(px(0.))
        .child(field_label(label, color))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .h(px(CONTROL_HEIGHT))
                .child(control),
        )
}

fn theme_toggle(is_dark: bool, cx: &mut Context<WrecApp>) -> impl IntoElement {
    KitButton::new("theme-mode")
        .ghost()
        .square(30.)
        .icon(if is_dark {
            PhosphorIcon::Moon
        } else {
            PhosphorIcon::Sun
        })
        .tooltip(if is_dark {
            "Switch to light mode"
        } else {
            "Switch to dark mode"
        })
        .on_click(cx.listener(move |_, _, window, cx| {
            let mode = if is_dark {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            change_theme(mode, Some(window), cx);
            cx.notify();
        }))
}

fn label_switch_row(label: &'static str, color: Hsla, switch: KitSwitch) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .gap_3()
        .child(field_label(label, color))
        .child(switch)
}

fn plain_info_row(label: &'static str, value: impl Into<SharedString>, value_color: Hsla) -> Div {
    let value = value.into();

    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .min_h(px(CONTROL_HEIGHT))
        .gap_3()
        .child(row_label(label))
        .child(
            div()
                .min_w(px(0.))
                .font_family(GEIST_MONO_FONT_FAMILY)
                .text_size(px(12.))
                .text_color(value_color)
                .truncate()
                .child(value),
        )
}

pub(crate) fn fps_label(fps: FrameRate) -> &'static str {
    match fps {
        FrameRate::Fps60 => "60 FPS",
        FrameRate::Fps30 => "30 FPS",
    }
}

/// Build an app toast whose message can be copied: hovering the toast reveals
/// a copy button at its top right, next to the close button.
pub(crate) fn app_notification(message: impl Into<SharedString>) -> Notification {
    let message: SharedString = message.into();
    Notification::new().content(move |_, _, cx| {
        let copy_text = message.clone();
        let t = Tokens::get(cx);
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .flex_1()
                    .min_w(px(0.))
                    .child(message.clone()),
            )
            .child(
                div()
                    .invisible()
                    .group_hover("", |this| this.visible())
                    .child(
                        KitButton::new("copy-notification")
                            .ghost()
                            .square(22.)
                            .icon(PhosphorIcon::Clipboard)
                            .icon_size(12.)
                            .icon_color(t.ink_muted)
                            .tooltip("Copy message")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_text.to_string(),
                                ));
                            }),
                    ),
            )
            .into_any_element()
    })
}

pub(crate) fn push_app_notification(window: &mut Window, notification: Notification, cx: &mut App) {
    window.push_notification(
        notification
            .w(px(NOTIFICATION_WIDTH))
            .min_h(px(44.))
            .py_2p5()
            .pl_4()
            .pr(px(34.))
            .gap_2(),
        cx,
    );
}

pub(crate) fn configure_notifications(cx: &mut App) {
    let notification = &mut Theme::global_mut(cx).notification;
    notification.placement = Anchor::BottomRight;
    notification.margins.top = px(8.);
    notification.margins.right = px(8.);
    notification.margins.bottom = px(8.);
    notification.margins.left = px(8.);
}

pub(crate) fn change_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    match window {
        Some(window) => {
            Theme::change(mode, Some(&mut *window), cx);
            apply_wrec_theme(cx);
            window.refresh();
        }
        None => {
            Theme::change(mode, None, cx);
            apply_wrec_theme(cx);
        }
    }
}

/// Install the kit tokens for the active mode, then project them onto the
/// gpui-component `Theme` so the remaining stock widgets (text input,
/// notifications, dialogs) match the kit. Tokens are the single source of
/// truth; nothing here defines a color of its own.
fn apply_wrec_theme(cx: &mut App) {
    let dark = Theme::global(cx).mode.is_dark();
    Tokens::install(dark, cx);
    let t = Tokens::get(cx);

    let theme = Theme::global_mut(cx);
    theme.font_family = GEIST_FONT_FAMILY.into();
    theme.mono_font_family = GEIST_MONO_FONT_FAMILY.into();
    theme.font_size = px(14.);
    theme.radius = px(kit::RADIUS);
    theme.radius_lg = px(kit::RADIUS_MENU);
    theme.shadow = false;

    theme.background = t.surface;
    theme.foreground = t.ink;
    theme.popover = t.raised;
    theme.popover_foreground = t.ink;
    theme.primary = t.solid;
    theme.primary_hover = t.solid_hover;
    theme.primary_active = t.solid_active;
    theme.primary_foreground = t.on_solid;
    theme.secondary = t.control;
    theme.secondary_hover = t.control_hover;
    theme.secondary_active = t.control_active;
    theme.secondary_foreground = t.ink;
    theme.muted = t.control;
    theme.muted_foreground = t.ink_muted;
    theme.accent = t.control_hover;
    theme.accent_foreground = t.ink;
    theme.danger = t.accent;
    theme.danger_hover = t.accent_hover;
    theme.danger_active = t.accent_active;
    theme.danger_foreground = t.on_accent;
    theme.border = t.line;
    theme.input = t.line;
    theme.ring = t.line;
    theme.caret = t.ink;
    theme.sidebar = t.panel;
    theme.sidebar_foreground = t.ink;
    theme.sidebar_primary = t.solid;
    theme.sidebar_primary_foreground = t.on_solid;
    theme.sidebar_accent = t.control;
    theme.sidebar_accent_foreground = t.ink;
    theme.sidebar_border = t.line;
    theme.button_primary = t.solid;
    theme.button_primary_hover = t.solid_hover;
    theme.button_primary_active = t.solid_active;
    theme.button_primary_foreground = t.on_solid;
    theme.list_hover = t.control_hover;
    theme.list_active = t.control_hover;
    theme.list_active_border = t.line;
    theme.title_bar = t.surface;
    theme.title_bar_border = t.line;
    theme.switch = t.track_off;
    theme.switch_thumb = t.surface;
    theme.skeleton = t.control;
    theme.selection = t.selection;
    theme.link = t.ink;
    theme.link_hover = t.solid_hover;
    theme.link_active = t.solid_active;
}

pub(crate) fn target_key(target: &CaptureTarget) -> String {
    let kind = match target.kind {
        CaptureSourceKind::Display => "display",
        CaptureSourceKind::Window => "window",
    };
    format!("{kind}:{}", target.id)
}

fn metrics_label(metrics: &RecorderMetrics) -> String {
    format!(
        "{}s  {:.1} MB  {:.1} Mbps",
        metrics.elapsed_secs,
        metrics.output_bytes as f32 / 1_000_000.,
        metrics.estimated_bitrate_mbps
    )
}

fn zero_metrics_label() -> String {
    "0s  0.0 MB  0.0 Mbps".to_string()
}

pub(crate) fn resolution_label(resolution: Resolution) -> &'static str {
    match resolution {
        Resolution::Native => "Original",
        Resolution::R720p => "720p",
        Resolution::R1080p => "1080p",
        Resolution::R2k => "2K",
        Resolution::R4k => "4K",
    }
}
