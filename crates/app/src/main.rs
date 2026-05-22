mod app;
mod assets;
mod config;
mod platform;
mod ui;

use app::WrecApp;
use assets::WrecAssets;
use gpui::*;
use gpui_component::{Root, Theme, ThemeMode};
use gpui_platform::application;
use ui::{
    configure_notifications, WINDOW_HEIGHT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH, WINDOW_WIDTH,
};

fn main() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    application().with_assets(WrecAssets).run(|cx: &mut App| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Light, None, cx);
        configure_notifications(cx);
        cx.activate(true);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                cx,
            )),
            window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
            titlebar: None,
            window_background: WindowBackgroundAppearance::Blurred,
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                window.activate_window();
                window.set_window_title("wrec");
                let app = cx.new(|cx| WrecApp::new(window, cx));
                cx.new(|cx| Root::new(app, window, cx))
            })
            .expect("open window");
        })
        .detach();
    });
}
