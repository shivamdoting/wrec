use crate::{app::WrecApp, assets::PhosphorIcon};
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
use wrec_core::{CaptureSourceKind, CaptureTarget, FrameRate, RecorderMetrics};

pub(crate) type ControlSelect = SelectState<Vec<&'static str>>;
pub(crate) type TargetSelect = SelectState<Vec<TargetOption>>;

pub(crate) const CONTROL_HEIGHT: f32 = 32.;
pub(crate) const WINDOW_WIDTH: f32 = 430.;
pub(crate) const WINDOW_HEIGHT: f32 = 450.;
pub(crate) const WINDOW_MIN_WIDTH: f32 = 390.;
pub(crate) const WINDOW_MIN_HEIGHT: f32 = 420.;
pub(crate) const SOURCE_OPTIONS: [&str; 2] = ["Display", "Window"];
pub(crate) const CODEC_OPTIONS: [&str; 2] = ["HEVC", "H.264"];
pub(crate) const QUALITY_OPTIONS: [&str; 3] = ["Balanced", "Efficient", "High"];
pub(crate) const FPS_OPTIONS: [&str; 2] = ["30 FPS", "60 FPS"];

const TAB_HEIGHT: f32 = 32.;
const FIELD_LABEL_WIDTH: f32 = 96.;
const NOTIFICATION_WIDTH: f32 = 320.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppTab {
    General,
    Settings,
    About,
}

impl AppTab {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Settings => 1,
            Self::About => 2,
        }
    }

    pub(crate) fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Settings,
            2 => Self::About,
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
        let refresh_disabled = self.recorder_state.is_busy() || self.recorder_state.is_recording();

        div().h(px(TAB_HEIGHT)).child(
            TabBar::new("wrec-tabs")
                .large()
                .w_full()
                .selected_index(self.active_tab.index())
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
                    this.active_tab = AppTab::from_index(*index);
                    cx.notify();
                }))
                .child(Tab::new().child(tab_text("General")))
                .child(Tab::new().child(tab_text("Settings")))
                .child(Tab::new().child(tab_text("About"))),
        )
    }

    pub(crate) fn render_general_tab(
        &self,
        record_icon: PhosphorIcon,
        record_label: &'static str,
        record_tip: &'static str,
        record_is_idle: bool,
        record_disabled: bool,
        controls_disabled: bool,
        muted_foreground: Hsla,
        metrics_label: Option<String>,
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

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .justify_between()
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
                            .child(quality_row)
                            .child(frame_rate_row)
                            .child(cursor_row),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when_some(metrics_label, |this, label| {
                        this.child(
                            div()
                                .flex()
                                .justify_end()
                                .h(px(18.))
                                .text_sm()
                                .text_color(muted_foreground)
                                .child(label),
                        )
                    })
                    .child(
                        record_button(
                            record_icon,
                            record_label,
                            record_tip,
                            record_is_idle,
                            record_disabled,
                            cx,
                        )
                        .w_full(),
                    ),
            )
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
            .gap_2()
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
                        Theme::change(mode, Some(window), cx);
                        cx.notify();
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
                UiButton::new("choose-output-dir")
                    .outline()
                    .w_full()
                    .h(px(CONTROL_HEIGHT))
                    .icon(UiIcon::new(PhosphorIcon::FolderOpen).text_color(muted_foreground))
                    .label("Folder")
                    .tooltip("Choose output folder")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.choose_output_dir(window, cx);
                        cx.notify();
                    })),
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
        let (record_icon, record_label, record_tip, record_is_idle) =
            if self.recorder_state.is_recording() {
                (PhosphorIcon::Stop, "Stop", "Stop recording", false)
            } else {
                (PhosphorIcon::Record, "Rec", "Start recording", true)
            };
        let record_disabled = self.recorder_state.is_busy();
        let controls_disabled = record_disabled || self.recorder_state.is_recording();
        let metrics_label = self.metrics.as_ref().map(metrics_label);

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
                                        controls_disabled,
                                        muted_foreground,
                                        metrics_label,
                                        cx,
                                    )),
                                    AppTab::Settings => this.child(self.render_settings_tab(
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
