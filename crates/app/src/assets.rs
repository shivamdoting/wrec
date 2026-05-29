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
    FolderOpen,
    Github,
    Pause,
    Play,
    Record,
    Refresh,
    Shield,
    Stop,
}

impl IconNamed for PhosphorIcon {
    fn path(self) -> SharedString {
        match self {
            Self::FolderOpen => "icons/phosphor/folder-open.svg",
            Self::Github => "icons/phosphor/github-logo.svg",
            Self::Pause => "icons/phosphor/pause.svg",
            Self::Play => "icons/phosphor/play.svg",
            Self::Record => "icons/phosphor/record.svg",
            Self::Refresh => "icons/phosphor/arrows-clockwise.svg",
            Self::Shield => "icons/phosphor/shield.svg",
            Self::Stop => "icons/phosphor/stop.svg",
        }
        .into()
    }
}

pub(crate) struct WrecAssets;

impl AssetSource for WrecAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let svg = match path {
            "icons/phosphor/folder-open.svg" => phosphor_svgs::style::bold::FOLDER_OPEN,
            "icons/phosphor/github-logo.svg" => phosphor_svgs::style::bold::GITHUB_LOGO,
            "icons/phosphor/pause.svg" => phosphor_svgs::style::bold::PAUSE,
            "icons/phosphor/play.svg" => phosphor_svgs::style::bold::PLAY,
            "icons/phosphor/record.svg" => phosphor_svgs::style::bold::RECORD,
            "icons/phosphor/arrows-clockwise.svg" => phosphor_svgs::style::bold::ARROWS_CLOCKWISE,
            "icons/phosphor/shield.svg" => phosphor_svgs::style::bold::SHIELD,
            "icons/phosphor/stop.svg" => phosphor_svgs::style::bold::STOP,
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
