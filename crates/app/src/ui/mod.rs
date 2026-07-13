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
    button::{Button as UiButton, ButtonVariant, ButtonVariants as _},
    input::Input,
    label::Label,
    notification::Notification,
    select::{Select, SelectItem, SelectState},
    switch::Switch,
    ActiveTheme as _, Colorize as _, Disableable as _, Icon as UiIcon, Root, Sizable as _, Theme,
    ThemeMode, WindowExt as _,
};
use std::rc::Rc;

pub(crate) type ControlSelect = SelectState<Vec<&'static str>>;
pub(crate) type LimitedSelect = SelectState<Vec<LimitedOption>>;
pub(crate) type TargetSelect = SelectState<Vec<TargetOption>>;

pub(crate) const CONTROL_HEIGHT: f32 = 32.;
const RECORD_BUTTON_HEIGHT: f32 = 48.;
pub(crate) const WINDOW_WIDTH: f32 = 628.;
pub(crate) const WINDOW_HEIGHT: f32 = 540.;
pub(crate) const WINDOW_MIN_WIDTH: f32 = 608.;
pub(crate) const WINDOW_MIN_HEIGHT: f32 = 500.;
pub(crate) const SOURCE_OPTIONS: [&str; 2] = ["Display", "Window"];
pub(crate) const PILL_WIDTH: f32 = 116.;
pub(crate) const PILL_HEIGHT: f32 = 34.;
pub(crate) const PILL_BOTTOM_MARGIN: f32 = 24.;
pub(crate) const CODEC_OPTIONS: [&str; 2] = ["HEVC", "H.264"];
pub(crate) const QUALITY_OPTIONS: [&str; 3] = ["Balanced", "Efficient", "High"];

const SIDEBAR_WIDTH: f32 = 154.;
const TITLE_BAR_HEIGHT: f32 = 40.;
const NATIVE_WINDOW_CONTROLS_WIDTH: f32 = 72.;
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
    primary_hover: u32,
    primary_active: u32,
    primary_foreground: u32,
    secondary: u32,
    secondary_hover: u32,
    secondary_active: u32,
    secondary_foreground: u32,
    muted: u32,
    muted_foreground: u32,
    accent: u32,
    accent_foreground: u32,
    destructive: u32,
    destructive_foreground: u32,
    border: u32,
    input: u32,
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
    background: 0xffffff,
    foreground: 0x18181b,
    card: 0xffffff,
    card_foreground: 0x18181b,
    popover: 0xffffff,
    popover_foreground: 0x18181b,
    primary: 0x18181b,
    primary_hover: 0x333338,
    primary_active: 0x000000,
    primary_foreground: 0xfafafa,
    secondary: 0xededf0,
    secondary_hover: 0xe1e1e6,
    secondary_active: 0xd6d6dc,
    secondary_foreground: 0x18181b,
    muted: 0xf4f4f5,
    muted_foreground: 0x71717a,
    accent: 0xf4f4f5,
    accent_foreground: 0x18181b,
    destructive: 0xe5484d,
    destructive_foreground: 0xffffff,
    border: 0xe4e4e7,
    input: 0xe4e4e7,
    chart_1: 0x18181b,
    chart_2: 0xededf0,
    chart_3: 0xf4f4f5,
    chart_4: 0xd4d4d8,
    chart_5: 0x18181b,
    sidebar: 0xfafafa,
    sidebar_foreground: 0x18181b,
    sidebar_primary: 0x18181b,
    sidebar_primary_foreground: 0xfafafa,
    sidebar_accent: 0xf0f0f2,
    sidebar_accent_foreground: 0x18181b,
    sidebar_border: 0xededf0,
};

const DARK_PALETTE: ThemePalette = ThemePalette {
    background: 0x000000,
    foreground: 0xfafafa,
    card: 0x141416,
    card_foreground: 0xfafafa,
    popover: 0x141416,
    popover_foreground: 0xfafafa,
    primary: 0xfafafa,
    primary_hover: 0xe2e2e6,
    primary_active: 0xcacace,
    primary_foreground: 0x0a0a0a,
    secondary: 0x1f1f22,
    secondary_hover: 0x2b2b2f,
    secondary_active: 0x343438,
    secondary_foreground: 0xfafafa,
    muted: 0x161618,
    muted_foreground: 0xa1a1aa,
    accent: 0x1f1f22,
    accent_foreground: 0xfafafa,
    destructive: 0xe5484d,
    destructive_foreground: 0xffffff,
    border: 0x232326,
    input: 0x232326,
    chart_1: 0xfafafa,
    chart_2: 0x1f1f22,
    chart_3: 0x1f1f22,
    chart_4: 0x3f3f46,
    chart_5: 0xfafafa,
    sidebar: 0x0b0b0c,
    sidebar_foreground: 0xfafafa,
    sidebar_primary: 0xfafafa,
    sidebar_primary_foreground: 0x0a0a0a,
    sidebar_accent: 0x1f1f22,
    sidebar_accent_foreground: 0xfafafa,
    sidebar_border: 0x1c1c1f,
};

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
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = cx.theme().mode.is_dark();
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
            .border_color(cx.theme().border)
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
            .child(theme_toggle(is_dark, cx))
    }

    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
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
                        .large()
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
                .large()
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
                .large()
                .h(px(CONTROL_HEIGHT))
                .placeholder("Format")
                .disabled(controls_disabled),
        );
        let quality_row = labeled_select_row(
            "Preset",
            muted_foreground,
            Select::new(&self.quality_select)
                .large()
                .h(px(CONTROL_HEIGHT))
                .placeholder("Preset")
                .disabled(controls_disabled),
        );
        let resolution_row = labeled_select_row(
            "Resolution",
            muted_foreground,
            Select::new(&self.resolution_select)
                .large()
                .h(px(CONTROL_HEIGHT))
                .placeholder("Resolution")
                .disabled(controls_disabled),
        );
        let frame_rate_row = labeled_select_row(
            "Frame Rate",
            muted_foreground,
            Select::new(&self.fps_select)
                .large()
                .h(px(CONTROL_HEIGHT))
                .placeholder("Frame Rate")
                .disabled(controls_disabled),
        );
        let cursor_row = label_switch_row(
            "Cursor",
            muted_foreground,
            Switch::new("cursor-switch")
                .checked(self.settings.include_cursor)
                .color(switch_on_color(cx))
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
                .color(switch_on_color(cx))
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
                        Switch::new("microphone-switch")
                            .checked(self.settings.include_microphone)
                            .color(switch_on_color(cx))
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
                Switch::new("hide-window-switch")
                    .checked(self.settings.hide_wrec)
                    .color(switch_on_color(cx))
                    .tooltip("Hide wrec from recording")
                    .disabled(controls_disabled)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.set_hide_wrec(*checked, cx);
                    })),
            ))
            .child(label_switch_row(
                "Logs",
                muted_foreground,
                Switch::new("logs-switch")
                    .checked(self.show_nerd_logs)
                    .color(switch_on_color(cx))
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
                        UiButton::new("choose-output-dir")
                            .secondary()
                            .flex_1()
                            .h(px(CONTROL_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
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
                            .secondary()
                            .flex_1()
                            .h(px(CONTROL_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
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
                        UiButton::new("cli-copy-install")
                            .secondary()
                            .compact()
                            .h(px(CONTROL_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .icon(UiIcon::new(PhosphorIcon::Clipboard).text_color(muted_foreground))
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
                        UiButton::new("skill-install")
                            .secondary()
                            .compact()
                            .h(px(CONTROL_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .icon(UiIcon::new(PhosphorIcon::Download).text_color(muted_foreground))
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
                        UiButton::new("open-recordings-data-dir")
                            .secondary()
                            .compact()
                            .h(px(CONTROL_HEIGHT))
                            .font_weight(FontWeight::SEMIBOLD)
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
                    .secondary()
                    .w_full()
                    .h(px(CONTROL_HEIGHT))
                    .font_weight(FontWeight::SEMIBOLD)
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
        let foreground = cx.theme().foreground;
        let muted_foreground = cx.theme().muted_foreground;
        let background = cx.theme().background;
        let border = cx.theme().border;
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
            .on_action(cx.listener(WrecApp::on_quit_action))
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
            .text_size(px(15.))
            .font_weight(FontWeight::SEMIBOLD)
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

/// Toned-down "on" color for switches — pure-white (primary) reads too loud on
/// the pitch-black background, so use a soft gray in dark mode.
fn switch_on_color(cx: &Context<WrecApp>) -> Hsla {
    if cx.theme().mode.is_dark() {
        Hsla::from(rgb(0xb0b0b8))
    } else {
        cx.theme().primary
    }
}

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
    // Clean filled pill, inset from the sidebar edges, for the selected/hovered item.
    let active_bg = cx.theme().accent;
    let hover_bg = cx.theme().muted;
    let color = if item.active {
        cx.theme().sidebar_accent_foreground
    } else {
        cx.theme().sidebar_foreground
    };
    let icon_color = if item.active {
        cx.theme().sidebar_accent_foreground
    } else {
        cx.theme().muted_foreground
    };
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
                .h(px(CONTROL_HEIGHT))
                .px_2p5()
                .rounded_lg()
                .text_base()
                .font_weight(if item.active {
                    FontWeight::BOLD
                } else {
                    FontWeight::SEMIBOLD
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
                    UiIcon::new(item.icon)
                        .size(px(16.))
                        .text_color(icon_color)
                        .flex_shrink_0(),
                )
                .child(div().flex_1().min_w(px(0.)).truncate().child(item.label)),
        )
        .on_click(move |event, window, cx| {
            on_click(event, window, cx);
        })
}

fn permission_state_button(
    status: PermissionStatus,
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
        .secondary()
        .h(px(CONTROL_HEIGHT))
        .font_weight(FontWeight::SEMIBOLD)
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

fn mic_permission_button(
    id: &'static str,
    status: PermissionStatus,
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
        "Microphone permission granted"
    } else {
        "Grant Microphone access. If macOS does not prompt, enable Wrec in System Settings > Privacy & Security > Microphone"
    };
    let button = UiButton::new(id)
        .compact()
        .secondary()
        .h(px(CONTROL_HEIGHT))
        .font_weight(FontWeight::SEMIBOLD)
        .label(label)
        .tooltip(tooltip)
        .disabled(busy || status.is_granted())
        .on_click(cx.listener(|this, _, _, cx| {
            this.request_microphone_permission(cx);
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
    // Recording keeps the solid primary button so stop carries the same
    // weight as record; the red stop glyph is the destructive accent.
    UiButton::new("record-button")
        .primary()
        .large()
        .h(px(RECORD_BUTTON_HEIGHT))
        .font_weight(FontWeight::SEMIBOLD)
        .icon(UiIcon::new(icon).text_color(if is_idle {
            theme.button_primary_foreground
        } else {
            theme.danger
        }))
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
) -> UiButton {
    UiButton::new("pause-button")
        .with_variant(ButtonVariant::Default)
        .large()
        .h(px(RECORD_BUTTON_HEIGHT))
        .font_weight(FontWeight::SEMIBOLD)
        .icon(UiIcon::new(icon).text_color(cx.theme().foreground))
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
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color),
    )
}

fn row_label(label: &'static str) -> Div {
    div()
        .text_base()
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
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
    UiButton::new("theme-mode")
        .ghost()
        .compact()
        .size(px(30.))
        .icon(UiIcon::new(if is_dark {
            PhosphorIcon::Moon
        } else {
            PhosphorIcon::Sun
        }))
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

/// Floating bottom-center indicator shown while a recording captures the
/// microphone. Lives in its own always-on-top popup window so it stays
/// visible when the main window is out of the way.
pub(crate) struct RecordingPill;

impl Render for RecordingPill {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(26.))
                    .px_3()
                    .rounded_full()
                    .bg(rgba(0x111111e6))
                    .border_1()
                    .border_color(rgba(0xffffff26))
                    .child(div().w_2().h_2().rounded_full().bg(rgb(0xe5484d)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_family(GEIST_FONT_FAMILY)
                            .text_color(rgb(0xffffff))
                            .child("Mic on"),
                    ),
            )
    }
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

/// Build an app toast whose message can be copied: hovering the toast reveals
/// a copy button at its top right, next to the close button.
pub(crate) fn app_notification(message: impl Into<SharedString>) -> Notification {
    let message: SharedString = message.into();
    Notification::new().content(move |_, _, cx| {
        let copy_text = message.clone();
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
                        UiButton::new("copy-notification")
                            .icon(
                                UiIcon::new(PhosphorIcon::Clipboard)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .ghost()
                            .xsmall()
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
    theme.font_size = px(14.);
    // Flat, clean look: rounded-rectangle corners and no shadows anywhere.
    theme.radius = px(8.);
    theme.radius_lg = px(12.);
    theme.shadow = false;

    theme.background = color(palette.background);
    theme.foreground = color(palette.foreground);
    theme.group_box = color(palette.card);
    theme.group_box_foreground = color(palette.card_foreground);
    theme.popover = color(palette.popover);
    theme.popover_foreground = color(palette.popover_foreground);
    theme.primary = color(palette.primary);
    // Explicit hover/active ramp, following gpui-component's own convention
    // (filled buttons shift their own lightness on interaction) instead of a
    // derived mix — `primary` equals `foreground` here so a mix would be a no-op.
    theme.primary_hover = color(palette.primary_hover);
    theme.primary_active = color(palette.primary_active);
    theme.primary_foreground = color(palette.primary_foreground);
    theme.secondary = color(palette.secondary);
    theme.secondary_hover = color(palette.secondary_hover);
    theme.secondary_active = color(palette.secondary_active);
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
    // Neutral focus ring / caret — no colored accent on focused controls.
    theme.ring = theme.border;
    theme.caret = theme.foreground;
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
    // Off-state track: `muted` is nearly invisible against the pitch-black
    // background, so use a clearly visible neutral gray in each mode.
    theme.switch = if theme.mode.is_dark() {
        color(0x3f3f46)
    } else {
        color(0xd4d4d8)
    };
    theme.switch_thumb = theme.popover;
    theme.skeleton = theme.muted;
    theme.slider_bar = theme.primary;
    theme.slider_thumb = theme.primary_foreground;
    theme.progress_bar = theme.primary;
    theme.selection = theme.foreground.opacity(0.14);
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
