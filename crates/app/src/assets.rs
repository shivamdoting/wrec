use gpui::{App, AssetSource, SharedString};
use gpui_component::IconNamed;
use std::borrow::Cow;

pub(crate) const GEIST_FONT_FAMILY: &str = "Geist";
pub(crate) const GEIST_MONO_FONT_FAMILY: &str = "Geist Mono";

pub(crate) fn register_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(&include_bytes!("../assets/fonts/geist/Geist[wght].ttf")[..]),
        Cow::Borrowed(&include_bytes!("../assets/fonts/geist/GeistMono[wght].ttf")[..]),
    ];

    if let Err(err) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("failed to register Geist fonts: {err}");
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PhosphorIcon {
    Clipboard,
    FilmReel,
    FolderOpen,
    Gauge,
    Gear,
    Github,
    Info,
    Moon,
    Pause,
    Play,
    Pulse,
    Refresh,
    Stop,
    Sun,
    Terminal,
}

impl IconNamed for PhosphorIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Clipboard => "icons/phosphor/clipboard-text.svg",
            Self::FilmReel => "icons/phosphor/film-reel.svg",
            Self::FolderOpen => "icons/phosphor/folder-open.svg",
            Self::Gauge => "icons/phosphor/gauge.svg",
            Self::Gear => "icons/phosphor/gear.svg",
            Self::Github => "icons/phosphor/github-logo.svg",
            Self::Info => "icons/phosphor/info.svg",
            Self::Moon => "icons/phosphor/moon.svg",
            Self::Pause => "icons/phosphor/pause.svg",
            Self::Play => "icons/phosphor/play.svg",
            Self::Pulse => "icons/phosphor/pulse.svg",
            Self::Refresh => "icons/phosphor/arrow-clockwise.svg",
            Self::Stop => "icons/phosphor/stop.svg",
            Self::Sun => "icons/phosphor/sun.svg",
            Self::Terminal => "icons/phosphor/terminal.svg",
        }
        .into()
    }
}

pub(crate) struct WrecAssets;

impl AssetSource for WrecAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let svg = match path {
            "icons/phosphor/clipboard-text.svg" => phosphor_svgs::style::bold::CLIPBOARD_TEXT,
            "icons/phosphor/film-reel.svg" => phosphor_svgs::style::bold::FILM_REEL,
            "icons/phosphor/folder-open.svg" => phosphor_svgs::style::bold::FOLDER_OPEN,
            "icons/phosphor/gauge.svg" => phosphor_svgs::style::bold::GAUGE,
            "icons/phosphor/gear.svg" => phosphor_svgs::style::bold::GEAR,
            "icons/phosphor/github-logo.svg" => phosphor_svgs::style::bold::GITHUB_LOGO,
            "icons/phosphor/info.svg" => phosphor_svgs::style::bold::INFO,
            "icons/phosphor/moon.svg" => phosphor_svgs::style::bold::MOON,
            "icons/phosphor/pause.svg" => phosphor_svgs::style::bold::PAUSE,
            "icons/phosphor/play.svg" => phosphor_svgs::style::bold::PLAY,
            "icons/phosphor/pulse.svg" => phosphor_svgs::style::bold::PULSE,
            "icons/phosphor/arrow-clockwise.svg" => phosphor_svgs::style::bold::ARROW_CLOCKWISE,
            "icons/phosphor/stop.svg" => phosphor_svgs::style::bold::STOP,
            "icons/phosphor/sun.svg" => phosphor_svgs::style::bold::SUN,
            "icons/phosphor/terminal.svg" => phosphor_svgs::style::bold::TERMINAL,
            "icons/chevron-down.svg" => phosphor_svgs::style::bold::CARET_DOWN,
            "icons/circle-check.svg" => phosphor_svgs::style::bold::CHECK_CIRCLE,
            "icons/circle-x.svg" => phosphor_svgs::style::bold::X_CIRCLE,
            "icons/close.svg" => phosphor_svgs::style::bold::X,
            "icons/info.svg" => phosphor_svgs::style::bold::INFO,
            "icons/triangle-alert.svg" => phosphor_svgs::style::bold::WARNING,
            _ => return Ok(None),
        };

        Ok(Some(Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
