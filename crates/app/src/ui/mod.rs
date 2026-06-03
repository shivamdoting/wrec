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
    ActiveTheme as _, Colorize as _, Disableable as _, Icon as UiIcon, Root, Theme, ThemeMode,
    WindowExt as _,
};
use std::rc::Rc;
use wrec_core::{
    CaptureSourceKind, CaptureTarget, FrameRate, Quality, RecorderMetrics, Resolution,
    ScreenRecordingPermissionStatus,
};

pub(crate) type ControlSelect = SelectState<Vec<&'static str>>;
pub(crate) type LimitedSelect = SelectState<Vec<LimitedOption>>;
pub(crate) type TargetSelect = SelectState<Vec<TargetOption>>;

pub(crate) const CONTROL_HEIGHT: f32 = 32.;
pub(crate) const WINDOW_WIDTH: f32 = 560.;
pub(crate) const WINDOW_HEIGHT: f32 = 540.;
pub(crate) const WINDOW_MIN_WIDTH: f32 = 540.;
pub(crate) const WINDOW_MIN_HEIGHT: f32 = 500.;
pub(crate) const SOURCE_OPTIONS: [&str; 2] = ["Display", "Window"];
pub(crate) const CODEC_OPTIONS: [&str; 2] = ["HEVC", "H.264"];
pub(crate) const QUALITY_OPTIONS: [&str; 3] = ["Balanced", "Efficient", "High"];

const SIDEBAR_WIDTH: f32 = 154.;
const SIDEBAR_LEFT_INSET: f32 = 16.;
const HEADER_HEIGHT: f32 = 48.;
const FIELD_LABEL_WIDTH: f32 = 96.;
const NOTIFICATION_WIDTH: f32 = 320.;

#[derive(Clone, Copy)]
struct ThemePalette {
    background: u32,
    foreground: u32,
    card: u32,
    card_foreground: u32,
    popover: u32,
    popover_foreground: u32,
    primary: u32,
    primary_foreground: u32,
    secondary: u32,
    secondary_foreground: u32,
    muted: u32,
    muted_foreground: u32,
    accent: u32,
    accent_foreground: u32,
    destructive: u32,
    destructive_foreground: u32,
    border: u32,
    input: u32,
    ring: u32,
    chart_1: u32,
    chart_2: u32,
    chart_3: u32,
    chart_4: u32,
    chart_5: u32,
    sidebar: u32,
    sidebar_foreground: u32,
    sidebar_primary: u32,
    sidebar_primary_foreground: u32,
    sidebar_accent: u32,
    sidebar_accent_foreground: u32,
    sidebar_border: u32,
}

const LIGHT_PALETTE: ThemePalette = ThemePalette {
    background: 0xf9f9f9,
    foreground: 0x202020,
    card: 0xfcfcfc,
    card_foreground: 0x202020,
    popover: 0xfcfcfc,
    popover_foreground: 0x202020,
    primary: 0x454956,
    primary_foreground: 0xffffff,
    secondary: 0xbab7e7,
    secondary_foreground: 0x2a3046,
    muted: 0xefefef,
    muted_foreground: 0x646464,
    accent: 0xe8e8e8,
    accent_foreground: 0x202020,
    destructive: 0x5272b3,
    destructive_foreground: 0xffffff,
    border: 0xd8d8d8,
    input: 0xd8d8d8,
    ring: 0x454956,
    chart_1: 0x454956,
    chart_2: 0xbab7e7,
    chart_3: 0xe8e8e8,
    chart_4: 0xc5c2eb,
    chart_5: 0x444957,
    sidebar: 0xfbfbfb,
    sidebar_foreground: 0x252525,
    sidebar_primary: 0x343434,
    sidebar_primary_foreground: 0xfbfbfb,
    sidebar_accent: 0xf7f7f7,
    sidebar_accent_foreground: 0x343434,
    sidebar_border: 0xebebeb,
};

const DARK_PALETTE: ThemePalette = ThemePalette {
    background: 0x0e0e0e,
    foreground: 0xeeeeee,
    card: 0x1b1b1b,
    card_foreground: 0xeeeeee,
    popover: 0x191919,
    popover_foreground: 0xeeeeee,
    primary: 0xc0c1ea,
    primary_foreground: 0x201a13,
    secondary: 0x2a2a32,
    secondary_foreground: 0xc0c1ea,
    muted: 0x202020,
    muted_foreground: 0xb4b4b4,
    accent: 0x2a2a2a,
    accent_foreground: 0xeeeeee,
    destructive: 0xcf3a3a,
    destructive_foreground: 0xffffff,
    border: 0x1a191c,
    input: 0x484848,
    ring: 0xc0c1ea,
    chart_1: 0xc0c1ea,
    chart_2: 0x2a2a32,
    chart_3: 0x2a2a2a,
    chart_4: 0x30303a,
    chart_5: 0xc0c0ea,
    sidebar: 0x1f1f1f,
    sidebar_foreground: 0xe8e9e8,
    sidebar_primary: 0x8ca148,
    sidebar_primary_foreground: 0xffffff,
    sidebar_accent: 0x262726,
    sidebar_accent_foreground: 0xe8e9e8,
    sidebar_border: 0x262726,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppTab {
    General,
    Settings,
    About,
    Nerd,
}

impl AppTab {
    fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Settings => "settings",
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

impl SelectItem for TargetOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.key
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

impl SelectItem for LimitedOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
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
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = [
            Some(sidebar_nav_item(
                "General",
                AppTab::General,
                AppTab::General.is_active(self.active_tab),
                cx,
            )),
            Some(sidebar_nav_item(
                "Settings",
                AppTab::Settings,
                AppTab::Settings.is_active(self.active_tab),
                cx,
            )),
            Some(sidebar_nav_item(
                "About",
                AppTab::About,
                AppTab::About.is_active(self.active_tab),
                cx,
            )),
            self.show_nerd_logs.then(|| {
                sidebar_nav_item(
                    "Nerd",
                    AppTab::Nerd,
                    AppTab::Nerd.is_active(self.active_tab),
                    cx,
                )
            }),
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
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(sidebar_header(cx.theme().mode.is_dark(), cx))
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
            "Preset",
            muted_foreground,
            Select::new(&self.quality_select)
                .h(px(CONTROL_HEIGHT))
                .placeholder("Preset")
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
            muted_foreground,
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
            muted_foreground,
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
                    .gap_2()
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let refresh_disabled = !self.permission_status.is_granted()
            || self.permission_busy
            || self.recorder_state.is_busy()
            || self.recorder_state.is_recording();

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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(row_label("Screen Recording"))
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .min_h(px(CONTROL_HEIGHT))
                    .child(row_label("Targets"))
                    .child(
                        UiButton::new("settings-refresh-targets")
                            .link()
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
            .child(switch_row(
                "Hide wrec",
                if self.settings.hide_wrec { "On" } else { "Off" },
                muted_foreground,
                Switch::new("hide-window-switch")
                    .checked(self.settings.hide_wrec)
                    .tooltip("Hide wrec from recording")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.set_hide_wrec(*checked, cx);
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
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(metrics_label),
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
                    .size_full()
                    .child(self.render_sidebar(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .pt_2()
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

fn sidebar_header(is_dark: bool, cx: &mut Context<WrecApp>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .h(px(HEADER_HEIGHT))
        .pl(px(SIDEBAR_LEFT_INSET))
        .pr_3()
        .child(div().size(px(22.)).bg(rgb(0xc62828)))
        .child(
            div()
                .flex_1()
                .h_full()
                .window_control_area(WindowControlArea::Drag),
        )
        .child(theme_toggle(is_dark, cx))
}

#[derive(Clone)]
struct WrecSidebarNav {
    items: Vec<WrecSidebarNavItem>,
}

#[derive(Clone)]
struct WrecSidebarNavItem {
    label: &'static str,
    tab: AppTab,
    active: bool,
    on_click: SidebarClickHandler,
}

impl WrecSidebarNav {
    fn render(self, id: impl Into<ElementId>, cx: &mut Context<WrecApp>) -> impl IntoElement {
        div()
            .id(id.into())
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .text_color(cx.theme().sidebar_foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.items.into_iter().map(|item| sidebar_nav_row(item, cx))),
            )
    }
}

fn sidebar_nav_item(
    label: &'static str,
    tab: AppTab,
    active: bool,
    cx: &mut Context<WrecApp>,
) -> WrecSidebarNavItem {
    let on_click = Rc::new(cx.listener(move |this, _, _, cx| {
        if this.active_tab != tab {
            this.active_tab = tab;
            cx.notify();
        }
    }));

    WrecSidebarNavItem {
        label,
        tab,
        active,
        on_click,
    }
}

fn sidebar_nav_row(item: WrecSidebarNavItem, cx: &mut Context<WrecApp>) -> impl IntoElement {
    let active_bg = if cx.theme().mode.is_dark() {
        Hsla::from(rgb(0x343434))
    } else {
        Hsla::from(rgb(0xe4e4e4))
    };
    let hover_bg = if cx.theme().mode.is_dark() {
        Hsla::from(rgb(0x2d2d2d))
    } else {
        Hsla::from(rgb(0xeeeeee))
    };
    let color = if item.active {
        cx.theme().sidebar_accent_foreground
    } else {
        cx.theme().sidebar_foreground
    };
    let on_click = item.on_click.clone();

    div()
        .id(format!("sidebar-nav-{}", item.tab.id()))
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .h(px(CONTROL_HEIGHT))
        .text_sm()
        .font_weight(if item.active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(color)
        .cursor_pointer()
        .when(item.active, |this| {
            this.bg(active_bg)
                .text_color(cx.theme().sidebar_accent_foreground)
        })
        .when(!item.active, |this| {
            this.hover(|this| {
                this.bg(hover_bg)
                    .text_color(cx.theme().sidebar_accent_foreground)
            })
        })
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .pl(px(SIDEBAR_LEFT_INSET))
                .pr_3()
                .truncate()
                .child(item.label),
        )
        .on_click(move |event, window, cx| {
            on_click(event, window, cx);
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

fn field_label(label: &'static str, color: Hsla) -> Div {
    div().w(px(FIELD_LABEL_WIDTH)).flex_none().child(
        Label::new(label)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color),
    )
}

fn row_label(label: &'static str) -> Div {
    div().text_sm().font_weight(FontWeight::MEDIUM).child(label)
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

fn theme_toggle(is_dark: bool, cx: &mut Context<WrecApp>) -> impl IntoElement {
    div()
        .id("theme-mode")
        .flex()
        .items_center()
        .justify_center()
        .size(px(CONTROL_HEIGHT))
        .cursor_pointer()
        .child(UiIcon::new(if is_dark {
            PhosphorIcon::Moon
        } else {
            PhosphorIcon::Sun
        }))
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
                .child(row_label(label))
                .child(div().text_sm().text_color(value_color).child(value)),
        )
        .child(switch)
}

fn label_switch_row(label: &'static str, color: Hsla, switch: Switch) -> Div {
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
    let palette = if theme.mode.is_dark() {
        DARK_PALETTE
    } else {
        LIGHT_PALETTE
    };
    let color = |hex| Hsla::from(rgb(hex));

    theme.font_family = GEIST_FONT_FAMILY.into();
    theme.mono_font_family = GEIST_MONO_FONT_FAMILY.into();
    theme.radius = px(8.);
    theme.radius_lg = px(8.);

    theme.background = color(palette.background);
    theme.foreground = color(palette.foreground);
    theme.group_box = color(palette.card);
    theme.group_box_foreground = color(palette.card_foreground);
    theme.popover = color(palette.popover);
    theme.popover_foreground = color(palette.popover_foreground);
    theme.primary = color(palette.primary);
    theme.primary_hover = theme.primary.mix(theme.foreground, 0.12);
    theme.primary_active = theme.primary.mix(theme.foreground, 0.2);
    theme.primary_foreground = color(palette.primary_foreground);
    theme.secondary = color(palette.secondary);
    theme.secondary_hover = theme.secondary.mix(theme.foreground, 0.08);
    theme.secondary_active = theme.secondary.mix(theme.foreground, 0.14);
    theme.secondary_foreground = color(palette.secondary_foreground);
    theme.muted = color(palette.muted);
    theme.muted_foreground = color(palette.muted_foreground);
    theme.accent = color(palette.accent);
    theme.accent_foreground = color(palette.accent_foreground);
    theme.danger = color(palette.destructive);
    theme.danger_hover = theme.danger.mix(theme.background, 0.14);
    theme.danger_active = theme.danger.mix(theme.background, 0.24);
    theme.danger_foreground = color(palette.destructive_foreground);
    theme.border = color(palette.border);
    theme.input = color(palette.input);
    theme.ring = theme.input;
    theme.caret = color(palette.ring);
    theme.chart_1 = color(palette.chart_1);
    theme.chart_2 = color(palette.chart_2);
    theme.chart_3 = color(palette.chart_3);
    theme.chart_4 = color(palette.chart_4);
    theme.chart_5 = color(palette.chart_5);
    theme.sidebar = color(palette.sidebar);
    theme.sidebar_foreground = color(palette.sidebar_foreground);
    theme.sidebar_primary = color(palette.sidebar_primary);
    theme.sidebar_primary_foreground = color(palette.sidebar_primary_foreground);
    theme.sidebar_accent = color(palette.sidebar_accent);
    theme.sidebar_accent_foreground = color(palette.sidebar_accent_foreground);
    theme.sidebar_border = color(palette.sidebar_border);
    theme.button_primary = theme.primary;
    theme.button_primary_hover = theme.primary_hover;
    theme.button_primary_active = theme.primary_active;
    theme.button_primary_foreground = theme.primary_foreground;
    theme.colors.list = theme.popover;
    theme.list_hover = theme.accent;
    theme.list_active = theme.accent;
    theme.list_active_border = theme.border;
    theme.list_even = theme.popover;
    theme.list_head = theme.muted;
    theme.table = color(palette.card);
    theme.table_head = theme.muted;
    theme.table_head_foreground = theme.muted_foreground;
    theme.table_hover = theme.accent;
    theme.table_active = theme.accent;
    theme.table_active_border = theme.border;
    theme.table_even = color(palette.card);
    theme.table_row_border = theme.border;
    theme.tiles = color(palette.card);
    theme.title_bar = theme.background;
    theme.title_bar_border = theme.border;
    theme.tab_bar = theme.background;
    theme.tab_bar_segmented = theme.muted;
    theme.tab = Hsla::transparent_black();
    theme.tab_active = theme.accent;
    theme.tab_active_foreground = theme.accent_foreground;
    theme.tab_foreground = theme.muted_foreground;
    theme.switch = theme.muted;
    theme.switch_thumb = theme.popover;
    theme.skeleton = theme.muted;
    theme.slider_bar = theme.primary;
    theme.slider_thumb = theme.primary_foreground;
    theme.progress_bar = theme.primary;
    theme.selection = color(palette.ring).opacity(0.24);
    theme.link = theme.primary;
    theme.link_hover = theme.primary_hover;
    theme.link_active = theme.primary_active;
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
