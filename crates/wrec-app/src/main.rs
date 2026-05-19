use gpui::*;
use gpui_platform::application;
use std::sync::{Arc, Mutex};
use wrec_core::{CaptureSourceKind, Codec, FrameRate, Quality, RecorderEngine, RecorderSettings};
use wrec_macos::MacosRecorder;

struct WrecApp {
    engine: Arc<Mutex<MacosRecorder>>,
    settings: RecorderSettings,
    status: String,
    is_recording: bool,
}

impl WrecApp {
    fn new() -> Self {
        Self {
            engine: Arc::new(Mutex::new(MacosRecorder::new())),
            settings: RecorderSettings::default(),
            status: "Ready".to_string(),
            is_recording: false,
        }
    }

    fn toggle_recording(&mut self) {
        if self.is_recording {
            match self.engine.lock().unwrap().stop() {
                Ok(()) => {
                    self.is_recording = false;
                    self.status = "Stopped".to_string();
                }
                Err(err) => self.status = err.to_string(),
            }
            return;
        }

        let target = match self.engine.lock().unwrap().list_targets() {
            Ok(targets) => targets
                .iter()
                .find(|target| target.kind == self.settings.source)
                .cloned()
                .or_else(|| targets.into_iter().next()),
            Err(err) => {
                self.status = err.to_string();
                return;
            }
        };

        let Some(target) = target else {
            self.status = "No capture target found".to_string();
            return;
        };

        match self
            .engine
            .lock()
            .unwrap()
            .start(target, self.settings.clone())
        {
            Ok(session) => {
                self.is_recording = true;
                self.status = format!("Recording to {}", session.output_path.display());
            }
            Err(err) => self.status = err.to_string(),
        }
    }
}

impl Render for WrecApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button_label = if self.is_recording { "Stop" } else { "Record" };

        div()
            .id("wrec-root")
            .size_full()
            .p_4()
            .bg(rgb(0x111111))
            .text_color(rgb(0xeeeeee))
            .font_family("-apple-system, BlinkMacSystemFont, sans-serif")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("wrec"),
                    )
                    .child(row("Source", source_label(self.settings.source)))
                    .child(row("FPS", fps_label(self.settings.fps)))
                    .child(row("Codec", codec_label(self.settings.codec)))
                    .child(row("Quality", quality_label(self.settings.quality)))
                    .child(row(
                        "Output",
                        self.settings.output_dir.display().to_string(),
                    ))
                    .child(button(button_label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _event, _window, cx| {
                            this.toggle_recording();
                            cx.notify();
                        }),
                    ))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xaaaaaa))
                            .child(self.status.clone()),
                    ),
            )
    }
}

fn row(label: impl Into<String>, value: impl Into<String>) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .child(div().text_color(rgb(0x999999)).child(label.into()))
        .child(div().child(value.into()))
}

fn button(label: impl Into<String>) -> Div {
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(0x2f6fed))
        .text_color(rgb(0xffffff))
        .cursor_pointer()
        .child(label.into())
}

fn source_label(source: CaptureSourceKind) -> &'static str {
    match source {
        CaptureSourceKind::Display => "Display",
        CaptureSourceKind::Window => "Window",
    }
}

fn fps_label(fps: FrameRate) -> String {
    format!("{} fps", fps.as_u32())
}

fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "HEVC",
        Codec::H264 => "H.264",
    }
}

fn quality_label(quality: Quality) -> &'static str {
    match quality {
        Quality::Efficient => "Efficient",
        Quality::Balanced => "Balanced",
        Quality::High => "High",
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.activate(true);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(380.), px(300.)),
                    cx,
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(|_| WrecApp::new()),
        )
        .expect("open window");
    });
}
