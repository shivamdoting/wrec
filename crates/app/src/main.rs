mod app;
mod assets;
mod platform;
mod ui;
mod updater;

use app::{Hide, Minimize, Quit, WrecApp};
use assets::{register_fonts, WrecAssets};
use gpui::*;
use gpui_component::Root;
use gpui_platform::application;
use ui::{
    change_theme, configure_notifications, WINDOW_HEIGHT, WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH,
    WINDOW_WIDTH,
};

fn main() {
    init_tracing();

    application().with_assets(WrecAssets).run(|cx: &mut App| {
        gpui_component::init(cx);
        register_fonts(cx);
        change_theme(gpui_component::ThemeMode::Light, None, cx);
        configure_notifications(cx);
        cx.activate(true);
        cx.bind_keys(app_key_bindings());

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                cx,
            )),
            window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
            is_resizable: false,
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(options, |window, cx| {
                window.activate_window();
                window.set_window_title("wrec");
                let app = cx.new(|cx| WrecApp::new(window, cx));
                window.on_window_should_close(cx, {
                    let app = app.downgrade();
                    move |_, cx| {
                        app.update_in(cx, |app, window, cx| app.request_quit(window, cx))
                            .unwrap_or(true)
                    }
                });
                cx.new(|cx| Root::new(app, window, cx))
            })
            .expect("open window");
        })
        .detach();
    });
}

fn init_tracing() {
    config::init_file_tracing(&config::log_path());
}

fn app_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]
}

#[cfg(test)]
mod tests {
    use super::app_key_bindings;
    use crate::app::{Hide, Minimize, Quit};
    use gpui::{Action, KeyBinding, Keystroke};

    #[test]
    fn app_key_bindings_include_minimize_and_quit() {
        let bindings = app_key_bindings();

        assert!(has_binding::<Minimize>(&bindings, "cmd-m"));
        assert!(has_binding::<Hide>(&bindings, "cmd-h"));
        assert!(has_binding::<Quit>(&bindings, "cmd-q"));
    }

    fn has_binding<A: Action>(bindings: &[KeyBinding], keystroke: &str) -> bool {
        let keystroke = Keystroke::parse(keystroke).expect("valid keystroke");
        bindings.iter().any(|binding| {
            binding.action().as_any().is::<A>()
                && binding.match_keystrokes(&[keystroke.clone()]) == Some(false)
        })
    }
}
