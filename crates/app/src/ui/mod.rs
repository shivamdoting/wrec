use crate::{
    app::WrecApp,
    assets::{PhosphorIcon, GEIST_FONT_FAMILY, GEIST_MONO_FONT_FAMILY},
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::{Button as UiButton, ButtonVariants as _},
    input::Input,
    label::Label,
    notification::Notification,
    select::{Select, SelectItem, SelectState},
    switch::Switch,
    tab::{Tab, TabBar},
    ActiveTheme as _, Disableable as _, Icon as UiIcon, Root, Sizable as _, Theme, ThemeMode,
    WindowExt as _,
};
use wrec_core::{
    CaptureSourceKind, CaptureTarget, FrameRate, RecorderMetrics, Resolution,
    ScreenRecordingPermissionStatus,
};

pub(crate) type ControlSelect = SelectState<Vec<&'static str>>;
pub(crate) type TargetSelect = SelectState<Vec<TargetOption>>;

pub(crate) const CONTROL_HEIGHT: f32 = 32.;
pub(crate) const WINDOW_WIDTH: f32 = 430.;
pub(crate) const WINDOW_HEIGHT: f32 = 540.;
pub(crate) const WINDOW_MIN_WIDTH: f32 = 390.;
pub(crate) const WINDOW_MIN_HEIGHT: f32 = 500.;
pub(crate) const SOURCE_OPTIONS: [&str; 2] = ["Display", "Window"];
pub(crate) const CODEC_OPTIONS: [&str; 2] = ["HEVC", "H.264"];
pub(crate) const QUALITY_OPTIONS: [&str; 3] = ["Balanced", "Efficient", "High"];
pub(crate) const RESOLUTION_OPTIONS: [&str; 5] = ["Original", "4K", "2K", "1080p", "720p"];
pub(crate) const FPS_OPTIONS: [&str; 2] = ["30 FPS", "60 FPS"];

const TAB_HEIGHT: f32 = 32.;
const FIELD_LABEL_WIDTH: f32 = 96.;
const NOTIFICATION_WIDTH: f32 = 320.;
const WREC_WHITE: u32 = 0xf8f8f8;
const WREC_BLACK: u32 = 0x111111;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppTab {
    General,
    Settings,
    About,
    Nerd,
}

impl AppTab {
    pub(crate) fn index(self, show_nerd_logs: bool) -> usize {
        match (self, show_nerd_logs) {
            (Self::General, _) => 0,
            (Self::Settings, _) => 1,
            (Self::About, _) => 2,
            (Self::Nerd, true) => 3,
            (Self::Nerd, false) => 2,
        }
    }

    pub(crate) fn from_index(index: usize, show_nerd_logs: bool) -> Self {
        match (index, show_nerd_logs) {
            (0, _) => Self::General,
            (1, _) => Self::Settings,
            (2, _) => Self::About,
            (3, true) => Self::Nerd,
            _ => Self::General,
        }
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

impl SelectItem for TargetOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.key
    }
}

impl WrecApp {
    pub(crate) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let refresh_disabled = !self.permission_status.is_granted()
            || self.permission_busy
            || self.recorder_state.is_busy()
            || self.recorder_state.is_recording();

        div().h(px(TAB_HEIGHT)).child(
            TabBar::new("wrec-tabs")
                .large()
                .w_full()
                .selected_index(self.active_tab.index(self.show_nerd_logs))
                .last_empty_space(
                    div()
                        .flex_1()
                        .h(px(TAB_HEIGHT))
                        .window_control_area(WindowControlArea::Drag),
                )
                .suffix(
                    div().flex().items_center().h(px(TAB_HEIGHT)).pr_2().child(
                        UiButton::new("refresh-targets")
                            .ghost()
                            .compact()
                            .size(px(28.))
                            .icon(UiIcon::new(PhosphorIcon::Refresh))
                            .tooltip("Refresh capture targets")
                            .disabled(refresh_disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_targets(cx);
                            })),
                    ),
                )
                .on_click(cx.listener(|this, index: &usize, _, cx| {
                    this.active_tab = AppTab::from_index(*index, this.show_nerd_logs);
                    cx.notify();
                }))
                .child(Tab::new().child(tab_text("General")))
                .child(Tab::new().child(tab_text("Settings")))
                .child(Tab::new().child(tab_text("About")))
                .when(self.show_nerd_logs, |this| {
                    this.child(Tab::new().child(tab_text("Nerd")))
                }),
        )
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
        let source_row = div()
            .flex()
            .items_center()
            .gap_3()
            .min_w(px(0.))
            .child(field_label("Source", muted_foreground))
            .child(
                div().flex_1().min_w(px(0.)).h(px(CONTROL_HEIGHT)).child(
                    Select::new(&self.source_select)
                        .h(px(CONTROL_HEIGHT))
                        .placeholder("Source")
                        .menu_max_h(rems(7.))
                        .disabled(controls_disabled),
                ),
            );
        let target_row = labeled_select_row(
            "Target",
            muted_foreground,
            Select::new(&self.target_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Target")
                .search_placeholder("Search targets")
                .menu_max_h(rems(14.))
                .disabled(controls_disabled),
        );
        let format_row = labeled_select_row(
            "Format",
            muted_foreground,
            Select::new(&self.codec_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Format")
                .disabled(controls_disabled),
        );
        let quality_row = labeled_select_row(
            "Quality",
            muted_foreground,
            Select::new(&self.quality_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Quality")
                .disabled(controls_disabled),
        );
        let resolution_row = labeled_select_row(
            "Resolution",
            muted_foreground,
            Select::new(&self.resolution_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Resolution")
                .disabled(controls_disabled),
        );
        let frame_rate_row = labeled_select_row(
            "Frame Rate",
            muted_foreground,
            Select::new(&self.fps_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Frame Rate")
                .disabled(controls_disabled),
        );
        let cursor_row = label_switch_row(
            "Cursor",
            Switch::new("cursor-switch")
                .checked(self.settings.include_cursor)
                .tooltip("Capture cursor")
                .disabled(controls_disabled)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.set_include_cursor(*checked, cx);
                })),
        );
        let audio_row = label_switch_row(
            "System Audio",
            Switch::new("system-audio-switch")
                .checked(self.settings.include_system_audio)
                .tooltip("Capture system audio")
                .disabled(controls_disabled)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.set_include_system_audio(*checked, cx);
                })),
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
                    .gap_3()
                    .child(source_row)
                    .child(target_row)
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
                            .child(audio_row),
                    ),
            )
            .child(if show_pause_button {
                div()
                    .flex()
                    .gap_2()
                    .child(
                        pause_button(pause_icon, pause_label, pause_tip, pause_disabled, cx)
                            .flex_1(),
                    )
                    .child(
                        record_button(
                            record_icon,
                            record_label,
                            record_tip,
                            record_is_idle,
                            record_disabled,
                            cx,
                        )
                        .flex_1(),
                    )
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
        is_dark: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Screen Recording"),
                            )
                            .child(
                                UiButton::new("settings-retry-screen-recording")
                                    .compact()
                                    .ghost()
                                    .size(px(28.))
                                    .icon(UiIcon::new(PhosphorIcon::Shield))
                                    .tooltip("Recheck Screen Recording permission")
                                    .disabled(self.permission_busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_permission_status(false, cx);
                                    })),
                            ),
                    )
                    .child(permission_state_button(
                        self.permission_status,
                        self.permission_busy,
                        cx,
                    )),
            )
            .child(switch_row(
                "Theme",
                if is_dark { "Dark" } else { "Light" },
                muted_foreground,
                Switch::new("theme-mode")
                    .checked(is_dark)
                    .tooltip("Switch theme")
                    .on_click(cx.listener(|_, checked, window, cx| {
                        let mode = if *checked {
                            ThemeMode::Dark
                        } else {
                            ThemeMode::Light
                        };
                        change_theme(mode, Some(window), cx);
                        cx.notify();
                    })),
            ))
            .child(switch_row(
                "Logs",
                if self.show_nerd_logs { "On" } else { "Off" },
                muted_foreground,
                Switch::new("logs-switch")
                    .checked(self.show_nerd_logs)
                    .tooltip("Show Nerd tab")
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.set_show_nerd_logs(*checked, cx);
                    })),
            ))
            .child(
                div().w_full().h(px(CONTROL_HEIGHT)).child(
                    Input::new(&self.output_input)
                        .h(px(CONTROL_HEIGHT))
                        .disabled(controls_disabled),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        UiButton::new("choose-output-dir")
                            .outline()
                            .flex_1()
                            .h(px(CONTROL_HEIGHT))
                            .icon(
                                UiIcon::new(PhosphorIcon::FolderOpen).text_color(muted_foreground),
                            )
                            .label("Choose")
                            .tooltip("Choose output folder")
                            .disabled(controls_disabled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_output_dir(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        UiButton::new("open-last-recording-dir")
                            .outline()
                            .flex_1()
                            .h(px(CONTROL_HEIGHT))
                            .icon(
                                UiIcon::new(PhosphorIcon::FolderOpen).text_color(muted_foreground),
                            )
                            .label("Open")
                            .tooltip("Open last recording folder")
                            .disabled(self.last_recording_dir.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_last_recording_dir(window, cx);
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
        div()
            .flex()
            .flex_col()
            .gap_3()
            .flex_1()
            .min_h(px(0.))
            .child(nerd_section_title(
                "Metrics",
                muted_foreground,
                metrics_label,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Logs"))
                    .child(
                        UiButton::new("open-recordings-data-dir")
                            .outline()
                            .compact()
                            .h(px(CONTROL_HEIGHT))
                            .icon(
                                UiIcon::new(PhosphorIcon::FolderOpen).text_color(muted_foreground),
                            )
                            .label("Open")
                            .tooltip("Open recordings data folder")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_recordings_data_dir(window, cx);
                            })),
                    ),
            )
    }

    pub(crate) fn render_about_tab(
        &self,
        muted_foreground: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                UiButton::new("open-github")
                    .outline()
                    .w_full()
                    .h(px(CONTROL_HEIGHT))
                    .icon(UiIcon::new(PhosphorIcon::Github).text_color(muted_foreground))
                    .label("GitHub")
                    .tooltip("Open GitHub repository")
                    .on_click(cx.listener(|this, _, window, cx| {
                        match crate::platform::open_url(crate::app::GITHUB_URL) {
                            Ok(()) => this.push_log("opened GitHub repository"),
                            Err(err) => {
                                this.push_log(format!("open GitHub failed: {err}"));
                                push_app_notification(
                                    window,
                                    Notification::new().message(format!(
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
        let foreground = cx.theme().foreground;
        let muted_foreground = cx.theme().muted_foreground;
        let background = cx.theme().background;
        let border = cx.theme().border;
        let is_dark = cx.theme().mode.is_dark();
        let notification_layer = Root::render_notification_layer(window, cx);
        let active_session = self.recorder_state.is_active_session();
        let (record_icon, record_label, record_tip, record_is_idle) = if active_session {
            (PhosphorIcon::Stop, "Stop", "Stop recording", false)
        } else {
            (PhosphorIcon::Record, "Rec", "Start recording", true)
        };
        let (pause_icon, pause_label, pause_tip) = if self.recorder_state.is_paused() {
            (PhosphorIcon::Play, "Resume", "Resume recording")
        } else {
            (PhosphorIcon::Pause, "Pause", "Pause recording")
        };
        let record_disabled = matches!(
            self.recorder_state,
            crate::app::RecorderState::CountingDown
                | crate::app::RecorderState::Starting
                | crate::app::RecorderState::Pausing
                | crate::app::RecorderState::Resuming
                | crate::app::RecorderState::Stopping
        ) || (!active_session
            && (self.permission_busy || !self.permission_status.is_granted()));
        let pause_disabled = matches!(
            self.recorder_state,
            crate::app::RecorderState::Pausing | crate::app::RecorderState::Resuming
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
            .relative()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(self.render_tabs(cx))
                    .child(
                        div().flex().flex_col().flex_1().pt_4().pb_4().px_4().child(
                            div().id("tab-content").flex().flex_col().flex_1().map(
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
                                        is_dark,
                                        cx,
                                    )),
                                    AppTab::Nerd if self.show_nerd_logs => this.child(
                                        self.render_nerds_tab(metrics_label, muted_foreground, cx),
                                    ),
                                    AppTab::Nerd => this.child(self.render_settings_tab(
                                        controls_disabled,
                                        muted_foreground,
                                        is_dark,
                                        cx,
                                    )),
                                    AppTab::About => {
                                        this.child(self.render_about_tab(muted_foreground, cx))
                                    }
                                },
                            ),
                        ),
                    ),
            )
            .children(notification_layer)
    }
}

fn nerd_section_title(title: &'static str, muted_foreground: Hsla, detail: Option<String>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(div().font_weight(FontWeight::MEDIUM).child(title))
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .text_sm()
                    .text_color(muted_foreground)
                    .truncate()
                    .child(detail),
            )
        })
}

fn permission_state_button(
    status: ScreenRecordingPermissionStatus,
    busy: bool,
    cx: &mut Context<WrecApp>,
) -> UiButton {
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
    let button = UiButton::new("settings-screen-recording-state")
        .compact()
        .outline()
        .h(px(CONTROL_HEIGHT))
        .label(label)
        .tooltip(tooltip)
        .disabled(busy || status.is_granted())
        .on_click(cx.listener(|this, _, _, cx| {
            this.request_screen_recording_permission(cx);
        }));

    if !busy && !status.is_granted() {
        button.primary()
    } else {
        button
    }
}

fn record_button(
    icon: PhosphorIcon,
    label: &'static str,
    tooltip: &'static str,
    is_idle: bool,
    disabled: bool,
    cx: &mut Context<WrecApp>,
) -> UiButton {
    let theme = cx.theme();
    let button = UiButton::new("record-button")
        .h(px(CONTROL_HEIGHT))
        .icon(UiIcon::new(icon).text_color(if is_idle {
            theme.button_primary_foreground
        } else {
            theme.danger_foreground
        }))
        .label(label)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_recording(window, cx);
            cx.notify();
        }));

    if is_idle {
        button.primary()
    } else {
        button.danger()
    }
}

fn pause_button(
    icon: PhosphorIcon,
    label: &'static str,
    tooltip: &'static str,
    disabled: bool,
    cx: &mut Context<WrecApp>,
) -> UiButton {
    UiButton::new("pause-button")
        .outline()
        .h(px(CONTROL_HEIGHT))
        .icon(UiIcon::new(icon).text_color(cx.theme().muted_foreground))
        .label(label)
        .tooltip(tooltip)
        .disabled(disabled)
        .on_click(cx.listener(|this, _, window, cx| {
            this.toggle_pause(window, cx);
            cx.notify();
        }))
}

fn tab_text(label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn field_label(label: &'static str, color: Hsla) -> Div {
    div().w(px(FIELD_LABEL_WIDTH)).flex_none().child(
        Label::new(label)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color),
    )
}

fn labeled_select_row(label: &'static str, color: Hsla, select: impl IntoElement) -> Div {
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
                .child(select),
        )
}

fn switch_row(label: &'static str, value: &'static str, value_color: Hsla, switch: Switch) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .gap_3()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .min_w(px(0.))
                .child(div().font_weight(FontWeight::MEDIUM).child(label))
                .child(div().text_color(value_color).child(value)),
        )
        .child(switch)
}

fn label_switch_row(label: &'static str, switch: Switch) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .gap_3()
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
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
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
        .child(
            div()
                .min_w(px(0.))
                .text_sm()
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

fn apply_wrec_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let white: Hsla = rgb(WREC_WHITE).into();
    let black: Hsla = rgb(WREC_BLACK).into();

    theme.font_family = GEIST_FONT_FAMILY.into();
    theme.mono_font_family = GEIST_MONO_FONT_FAMILY.into();

    if theme.mode.is_dark() {
        theme.background = black;
        theme.foreground = white.opacity(0.8);
        theme.muted_foreground = white.opacity(0.56);
        theme.popover = rgb(0x29333c).into();
        theme.popover_foreground = theme.foreground;
        theme.border = white.opacity(0.12);
        theme.input = white.opacity(0.16);
        theme.ring = theme.input;
        theme.muted = white.opacity(0.08);
        theme.accent = white.opacity(0.08);
        theme.accent_foreground = theme.foreground;
        theme.secondary = white.opacity(0.08);
        theme.secondary_foreground = theme.foreground;
        theme.primary = white;
        theme.primary_hover = rgb(0xececec).into();
        theme.primary_active = rgb(0xdedede).into();
        theme.primary_foreground = black.opacity(0.8);
        theme.button_primary = theme.primary;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = theme.primary_foreground;
        theme.link = theme.foreground;
        theme.link_hover = white;
        theme.link_active = white;
        theme.tab_bar = black;
        theme.tab_bar_segmented = white.opacity(0.08);
        theme.tab_active = white.opacity(0.1);
        theme.tab_active_foreground = theme.foreground;
        theme.tab_foreground = theme.muted_foreground;
        theme.title_bar = black;
        theme.title_bar_border = theme.border;
        theme.switch = white.opacity(0.18);
        theme.switch_thumb = white;
        theme.group_box = white.opacity(0.06);
        theme.group_box_foreground = theme.foreground;
        theme.caret = white;
    } else {
        theme.background = white;
        theme.foreground = black.opacity(0.8);
        theme.muted_foreground = black.opacity(0.56);
        theme.popover = white;
        theme.popover_foreground = theme.foreground;
        theme.border = black.opacity(0.12);
        theme.input = black.opacity(0.16);
        theme.ring = theme.input;
        theme.muted = black.opacity(0.06);
        theme.accent = black.opacity(0.06);
        theme.accent_foreground = theme.foreground;
        theme.secondary = black.opacity(0.06);
        theme.secondary_foreground = theme.foreground;
        theme.primary = black;
        theme.primary_hover = rgb(0x2b3640).into();
        theme.primary_active = rgb(0x1b232a).into();
        theme.primary_foreground = white.opacity(0.8);
        theme.button_primary = theme.primary;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.button_primary_foreground = theme.primary_foreground;
        theme.link = theme.foreground;
        theme.link_hover = black;
        theme.link_active = black;
        theme.tab_bar = white;
        theme.tab_bar_segmented = black.opacity(0.06);
        theme.tab_active = black.opacity(0.08);
        theme.tab_active_foreground = theme.foreground;
        theme.tab_foreground = theme.muted_foreground;
        theme.title_bar = white;
        theme.title_bar_border = theme.border;
        theme.switch = black.opacity(0.16);
        theme.switch_thumb = white;
        theme.group_box = black.opacity(0.04);
        theme.group_box_foreground = theme.foreground;
        theme.caret = black;
    }
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
