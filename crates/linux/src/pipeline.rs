use crate::{
    backend,
    encoding::{self, Mode},
    portal::PipeWireStream,
    Command,
};
use domain::{
    CaptureDimensions, Codec, RecorderError, RecorderEvent, RecorderMetrics, RecorderSettings,
    RecordingSession, Resolution, Result,
};
use gstreamer::{self as gst, prelude::*};
use std::{
    os::fd::AsRawFd,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub(crate) fn element(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().map_err(|error| backend(format!("GStreamer element {factory} is unavailable: {error}. Install the Linux runtime dependencies in packaging/linux/README.md.")))
}

pub(crate) fn check_plugins(settings: &RecorderSettings) -> Result<()> {
    gst::init().map_err(backend)?;
    for factory in [
        "capsfilter",
        "queue",
        parser_name(settings.codec),
        "qtmux",
        "filesink",
    ] {
        element(factory)?;
    }
    if settings.include_system_audio || settings.include_microphone {
        for factory in [
            "pulsesrc",
            "audioconvert",
            "audioresample",
            "avenc_aac",
            "aacparse",
        ] {
            element(factory)?;
        }
    }
    Ok(())
}

fn parser_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264parse",
        Codec::Hevc => "h265parse",
    }
}

/// Fit inside the requested bounds without upscaling or odd chroma dimensions.
fn output_size(width: i32, height: i32, resolution: Resolution) -> (i32, i32) {
    let (max_width, max_height) = match resolution {
        Resolution::Native => (width, height),
        Resolution::R720p => (1280, 720),
        Resolution::R1080p => (1920, 1080),
        Resolution::R2k => (2560, 1440),
        Resolution::R4k => (3840, 2160),
    };
    let scale = (max_width as f64 / width as f64)
        .min(max_height as f64 / height as f64)
        .min(1.0);
    let even = |size: i32| (((size as f64 * scale) as i32) & !1).max(2);
    (even(width), even(height))
}

fn video_queue() -> Result<gst::Element> {
    let queue = element("queue")?;
    queue.set_property("max-size-buffers", 2u32);
    queue.set_property("max-size-bytes", 0u32);
    queue.set_property("max-size-time", 0u64);
    queue.set_property_from_str("leaky", "downstream");
    Ok(queue)
}

struct PipelineGuard(gst::Pipeline);
impl Drop for PipelineGuard {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    dropped: AtomicU64,
    dimensions: Mutex<Option<CaptureDimensions>>,
    last_pts: Mutex<Option<gst::ClockTime>>,
}

pub(crate) enum CaptureInput {
    PipeWire(PipeWireStream),
    X11 { display: String, xid: u64 },
}

struct Attempt {
    mode: Mode,
    counters: Arc<Counters>,
}

pub(crate) fn record(
    capture: &CaptureInput,
    session: &RecordingSession,
    settings: &RecorderSettings,
    events: &mpsc::Sender<RecorderEvent>,
    commands: mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
) -> Result<()> {
    if matches!(capture, CaptureInput::X11 { .. }) {
        crate::x11::initialize()?;
    }
    check_plugins(settings)?;
    let modes = encoding::modes(settings.codec, matches!(capture, CaptureInput::PipeWire(_)));
    if modes.is_empty() {
        return Err(backend(format!("No {} encoder is installed. Install GStreamer VA/NVENC plugins or x264/x265/openh264 for software compatibility; see packaging/linux/README.md.", settings.codec.as_arg())));
    }
    let mut last_error = None;
    for mode in modes {
        if *stop.borrow() {
            return Err(RecorderError::Cancelled);
        }
        let attempt = Attempt {
            mode,
            counters: Arc::new(Counters::default()),
        };
        let _ = events.send(RecorderEvent::Log {
            session_id: Some(session.id),
            message: format!("capture-engine: trying {}", mode.description()),
        });
        let result = record_attempt(
            capture, session, settings, events, &commands, stop, &attempt,
        );
        if result.is_ok()
            || matches!(result, Err(RecorderError::Cancelled))
            || attempt.counters.frames.load(Ordering::Relaxed) > 0
        {
            return result;
        }
        let error = result.unwrap_err();
        let _ = events.send(RecorderEvent::Log {
            session_id: Some(session.id),
            message: format!("capture-engine: {} could not start: {error}", mode.factory),
        });
        last_error = Some(error);
    }
    Err(last_error.unwrap_or_else(|| backend("No recording pipeline could start")))
}

fn record_attempt(
    capture: &CaptureInput,
    session: &RecordingSession,
    settings: &RecorderSettings,
    events: &mpsc::Sender<RecorderEvent>,
    commands: &mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    attempt: &Attempt,
) -> Result<()> {
    if *stop.borrow() {
        return Err(RecorderError::Cancelled);
    }
    let settings = settings.clone().with_preset_limits();
    let pipeline = PipelineGuard(gst::Pipeline::new());
    pipeline.0.use_clock(Some(&gst::SystemClock::obtain()));
    let mode = attempt.mode;
    let source = match capture {
        CaptureInput::PipeWire(capture) => {
            let source = element("pipewiresrc")?;
            source.set_property("fd", capture.fd.as_raw_fd());
            source.set_property("path", capture.node.to_string());
            source.set_property("always-copy", false);
            source.set_property("min-buffers", 4i32);
            source.set_property("max-buffers", 8i32);
            source.set_property("keepalive-time", 1000i32);
            source.set_property("resend-last", true);
            source
        }
        CaptureInput::X11 { display, xid } => {
            let source = element("ximagesrc")?;
            source.set_property("display-name", display);
            source.set_property("xid", *xid);
            source.set_property("show-pointer", settings.include_cursor);
            source.set_property("use-damage", true);
            source
        }
    };
    source.set_property("do-timestamp", true);
    let input = element("capsfilter")?;
    let caps = gst::Caps::builder("video/x-raw").field(
        "framerate",
        gst::Fraction::new(settings.fps.as_u32() as i32, 1),
    );
    let caps = if mode.dmabuf {
        caps.features(["memory:DMABuf"]).build()
    } else {
        caps.build()
    };
    input.set_property("caps", caps);
    let queue = video_queue()?;
    let converters = mode.converters()?;
    let output = element("capsfilter")?;
    output.set_property("caps", mode.caps(None));
    let encoder = mode.encoder(&settings)?;
    let parser = element(parser_name(settings.codec))?;
    let mux = element("qtmux")?;
    mux.set_property("fragment-duration", 10000u32);
    // Preserve sub-frame timestamps around pause/resume instead of rounding
    // them to the default frame-rate-derived track timescale.
    mux.set_property("trak-timescale", 1_000_000u32);
    let sink = element("filesink")?;
    sink.set_property(
        "location",
        session
            .output_path
            .to_str()
            .ok_or_else(|| backend("recording output path must be UTF-8"))?,
    );
    sink.set_property("sync", false);
    let mut chain = vec![&source, &input, &queue];
    chain.extend(converters.iter());
    chain.extend([&output, &encoder, &parser, &mux, &sink]);
    pipeline
        .0
        .add_many(chain.iter().copied())
        .map_err(backend)?;
    gst::Element::link_many(chain.iter().copied()).map_err(backend)?;
    if settings.include_system_audio {
        add_audio(&pipeline.0, &mux, Some("@DEFAULT_MONITOR@"))?;
    }
    if settings.include_microphone {
        add_audio(&pipeline.0, &mux, None)?;
    }
    let counters = attempt.counters.clone();
    attach_capture_probe(
        &source,
        &output,
        settings.resolution,
        mode,
        counters.clone(),
    );
    attach_encoded_probe(&parser, counters.clone());
    // One overrun accompanies each incoming frame that replaces the oldest frame.
    let dropped = counters.clone();
    queue.connect("overrun", false, move |_| {
        dropped.dropped.fetch_add(1, Ordering::Relaxed);
        None
    });
    let _ = events.send(RecorderEvent::Log {
        session_id: Some(session.id),
        message: format!(
            "capture-engine: selected {}; video queue limited to 2 frames",
            mode.description()
        ),
    });
    // Reserve a unique file so a collision never truncates an existing recording.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&session.output_path)
        .map_err(backend)?;
    let result = run(&pipeline.0, session, events, commands, stop, &counters);
    let _ = pipeline.0.set_state(gst::State::Null);
    if matches!(result, Err(RecorderError::Cancelled))
        || counters.frames.load(Ordering::Relaxed) == 0
    {
        let _ = std::fs::remove_file(&session.output_path);
    }
    result
}

fn attach_encoded_probe(parser: &gst::Element, counters: Arc<Counters>) {
    parser
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counters.frames.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
}

fn add_audio(pipeline: &gst::Pipeline, mux: &gst::Element, device: Option<&str>) -> Result<()> {
    let source = element("pulsesrc")?;
    source.set_property("provide-clock", false);
    if let Some(device) = device {
        source.set_property("device", device);
    }
    add_audio_source(pipeline, mux, &source)
}

fn add_audio_source(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    source: &gst::Element,
) -> Result<()> {
    let convert = element("audioconvert")?;
    let resample = element("audioresample")?;
    let caps = element("capsfilter")?;
    caps.set_property(
        "caps",
        gst::Caps::builder("audio/x-raw")
            .field("rate", 48000i32)
            .field("channels", 2i32)
            .build(),
    );
    let encoder = element("avenc_aac")?;
    encoder.set_property("bitrate", 128000i32);
    let parser = element("aacparse")?;
    let queue = element("queue")?;
    queue.set_property("max-size-time", 2_000_000_000u64);
    queue.set_property("max-size-buffers", 0u32);
    queue.set_property("max-size-bytes", 0u32);
    let chain = [
        source, &convert, &resample, &caps, &encoder, &parser, &queue,
    ];
    pipeline.add_many(chain).map_err(backend)?;
    gst::Element::link_many(chain).map_err(backend)?;
    queue.link(mux).map_err(backend)?;
    Ok(())
}

fn attach_capture_probe(
    source: &gst::Element,
    output: &gst::Element,
    resolution: Resolution,
    mode: Mode,
    counters: Arc<Counters>,
) {
    let output = output.clone();
    source.static_pad("src").unwrap().add_probe(gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER, move |pad, info| {
        if let Some(gst::PadProbeData::Event(event)) = &info.data {
            if let gst::EventView::Caps(caps) = event.view() {
                if let Some(structure) = caps.caps().structure(0) {
                    if let (Ok(width), Ok(height)) = (structure.get::<i32>("width"), structure.get::<i32>("height")) {
                        if width >= 2 && height >= 2 {
                            let mut dimensions = counters.dimensions.lock().unwrap();
                            // A MOV track has a fixed canvas. Window resizes are
                            // scaled/letterboxed into the initial output size.
                            let (w, h) = dimensions.map(|d| (d.output_width as i32, d.output_height as i32))
                                .unwrap_or_else(|| output_size(width, height, resolution));
                            dimensions.get_or_insert(CaptureDimensions { native_width: width.into(), native_height: height.into(), output_width: w.into(), output_height: h.into() });
                            drop(dimensions);
                            output.set_property("caps", mode.caps(Some((w, h))));
                        }
                    }
                }
            }
        }
        if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
            if let Some(pts) = buffer.pts() {
                let mut previous = counters.last_pts.lock().unwrap();
                if previous.is_some_and(|last| pts <= last || pts.saturating_sub(last) < gst::ClockTime::from_useconds(1)) {
                    counters.dropped.fetch_add(1, Ordering::Relaxed);
                    return gst::PadProbeReturn::Drop;
                }
                *previous = Some(pts);
            }
            if mode.dmabuf && (buffer.n_memory() == 0 || buffer.iter_memories().any(|memory| !memory.is_memory_type::<gstreamer_allocators::DmaBufMemory>())) {
                if let Some(element) = pad.parent_element() {
                    gst::element_error!(element, gst::StreamError::Format, ("PipeWire delivered a non-DMA-BUF frame. This attempt requires GPU buffer sharing; another available mode will be tried before capture starts."));
                }
                return gst::PadProbeReturn::Drop;
            }
        }
        gst::PadProbeReturn::Ok
    });
}

fn set_state(pipeline: &gst::Pipeline, state: gst::State) -> Result<()> {
    pipeline.set_state(state).map_err(backend)?;
    let (result, current, _) = pipeline.state(gst::ClockTime::from_seconds(3));
    result.map_err(backend)?;
    if current != state {
        return Err(backend(format!(
            "recording did not enter {state:?} within 3s"
        )));
    }
    Ok(())
}

fn run(
    pipeline: &gst::Pipeline,
    session: &RecordingSession,
    events: &mpsc::Sender<RecorderEvent>,
    commands: &mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    counters: &Counters,
) -> Result<()> {
    pipeline.set_state(gst::State::Playing).map_err(backend)?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| backend("recording pipeline has no bus"))?;
    let launched = Instant::now();
    let mut started = false;
    let mut stopping = None;
    let mut last_metrics = Instant::now();
    loop {
        if *stop.borrow() && stopping.is_none() {
            if !started && counters.frames.load(Ordering::Relaxed) == 0 {
                return Err(RecorderError::Cancelled);
            }
            // Live sources must run to drain their pending buffers and finalize a paused movie.
            pipeline.set_state(gst::State::Playing).map_err(backend)?;
            if !pipeline.send_event(gst::event::Eos::new()) {
                return Err(backend("recording pipeline rejected finalization"));
            }
            stopping = Some(Instant::now());
        }
        if let Ok(command) = commands.try_recv() {
            let (state, reply) = match command {
                Command::Pause(reply) => (gst::State::Paused, reply),
                Command::Resume(reply) => (gst::State::Playing, reply),
            };
            let result = if stopping.is_some() || !started {
                Err(backend("recording is not ready for pause/resume"))
            } else {
                set_state(pipeline, state)
            };
            let _ = reply.send(result);
        }
        if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    return Err(backend(format!(
                        "{}: {} ({})",
                        error
                            .src()
                            .map(|src| src.name().to_string())
                            .unwrap_or_default(),
                        error.error(),
                        error.debug().unwrap_or_default()
                    )))
                }
                gst::MessageView::Eos(_) => {
                    if counters.frames.load(Ordering::Relaxed) == 0 {
                        return Err(backend("capture ended without an encoded frame"));
                    }
                    emit_metrics(pipeline, session, events, counters);
                    return Ok(());
                }
                _ => {}
            }
        }
        if !started && counters.frames.load(Ordering::Relaxed) > 0 {
            started = true;
            let _ = events.send(RecorderEvent::Started {
                session_id: session.id,
                dimensions: *counters.dimensions.lock().unwrap(),
            });
        }
        if !started && launched.elapsed() > Duration::from_secs(15) {
            return Err(backend("No encoded frame arrived within 15s. Check DMA-BUF support, VA-API driver access, and audio devices."));
        }
        if stopping.is_some_and(|at| at.elapsed() > Duration::from_secs(10)) {
            return Err(backend(
                "Movie finalization timed out after 10s; only completed fragments may be playable.",
            ));
        }
        if started && last_metrics.elapsed() >= Duration::from_secs(1) {
            emit_metrics(pipeline, session, events, counters);
            last_metrics = Instant::now();
        }
    }
}

fn emit_metrics(
    pipeline: &gst::Pipeline,
    session: &RecordingSession,
    events: &mpsc::Sender<RecorderEvent>,
    counters: &Counters,
) {
    let elapsed_secs = pipeline
        .query_position::<gst::ClockTime>()
        .map(|time| time.seconds())
        .unwrap_or(0);
    let output_bytes = file_size(&session.output_path);
    let _ = events.send(RecorderEvent::Metrics {
        session_id: session.id,
        metrics: RecorderMetrics {
            elapsed_secs,
            output_bytes,
            estimated_bitrate_mbps: if elapsed_secs > 0 {
                output_bytes as f32 * 8.0 / elapsed_secs as f32 / 1_000_000.0
            } else {
                0.0
            },
            frames: Some(counters.frames.load(Ordering::Relaxed)),
            dropped_frames: Some(counters.dropped.load(Ordering::Relaxed)),
        },
    });
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_preserve_aspect_and_do_not_upscale() {
        assert_eq!(output_size(3840, 2160, Resolution::R1080p), (1920, 1080));
        assert_eq!(output_size(1080, 1920, Resolution::R1080p), (606, 1080));
        assert_eq!(output_size(800, 600, Resolution::R1080p), (800, 600));
        assert_eq!(output_size(1919, 1079, Resolution::Native), (1918, 1078));
    }

    struct TestRecording {
        pipeline: gst::Pipeline,
        session: RecordingSession,
        events: mpsc::Receiver<RecorderEvent>,
        commands: mpsc::SyncSender<Command>,
        stop: watch::Sender<bool>,
        worker: Option<std::thread::JoinHandle<Result<()>>>,
    }

    impl TestRecording {
        fn start(audio_tracks: usize) -> Self {
            gst::init().unwrap();
            static ID: AtomicU64 = AtomicU64::new(0);
            let id = ID.fetch_add(1, Ordering::Relaxed);
            let session = RecordingSession {
                id,
                output_path: std::env::temp_dir()
                    .join(format!("wrec-linux-test-{}-{id}.mov", std::process::id())),
            };
            // Synthetic software encoding is confined to tests. Exercise the same
            // bus/control/mux code without pretending this is a hardware benchmark.
            let pipeline = gst::parse::launch("videotestsrc is-live=true pattern=ball ! video/x-raw,width=320,height=180,framerate=30/1 ! openh264enc ! h264parse name=parser ! qtmux name=mux fragment-duration=10000 ! filesink name=output sync=false")
                .unwrap().downcast::<gst::Pipeline>().unwrap();
            pipeline
                .by_name("output")
                .unwrap()
                .set_property("location", session.output_path.to_str().unwrap());
            for _ in 0..audio_tracks {
                let source = element("audiotestsrc").unwrap();
                source.set_property("is-live", true);
                add_audio_source(&pipeline, &pipeline.by_name("mux").unwrap(), &source).unwrap();
            }
            let counters = Arc::new(Counters::default());
            attach_encoded_probe(&pipeline.by_name("parser").unwrap(), counters.clone());
            let (events_tx, events) = mpsc::channel();
            let (commands, commands_rx) = mpsc::sync_channel(1);
            let (stop, stopped) = watch::channel(false);
            let worker_pipeline = pipeline.clone();
            let worker_session = session.clone();
            let worker = std::thread::spawn(move || {
                let guard = PipelineGuard(worker_pipeline);
                run(
                    &guard.0,
                    &worker_session,
                    &events_tx,
                    &commands_rx,
                    &stopped,
                    &counters,
                )
            });
            let recording = Self {
                pipeline,
                session,
                events,
                commands,
                stop,
                worker: Some(worker),
            };
            loop {
                if matches!(
                    recording
                        .events
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap(),
                    RecorderEvent::Started { .. }
                ) {
                    break;
                }
            }
            recording
        }

        fn control(&self, pause: bool) {
            let (tx, rx) = mpsc::sync_channel(1);
            self.commands
                .send(if pause {
                    Command::Pause(tx)
                } else {
                    Command::Resume(tx)
                })
                .unwrap();
            rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        }

        fn finish(&mut self) -> Result<()> {
            self.stop.send(true).unwrap();
            self.worker.take().unwrap().join().unwrap()
        }

        fn probe(&self) -> serde_json::Value {
            let output = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-show_packets",
                    "-of",
                    "json",
                ])
                .arg(&self.session.output_path)
                .output()
                .expect("install ffmpeg to run Linux recording tests");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let probe: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let mut previous = std::collections::HashMap::new();
            for packet in probe["packets"].as_array().unwrap() {
                let stream = packet["stream_index"].as_u64().unwrap();
                let dts = packet["dts"].as_i64().unwrap();
                if let Some(last) = previous.insert(stream, dts) {
                    assert!(
                        dts > last,
                        "stream {stream} has non-increasing DTS: {last} -> {dts}"
                    );
                }
            }
            let decoded = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&self.session.output_path)
                // Preserve the movie's timestamp precision when decoding VFR
                // into null output; rounding to 1/30 can create duplicate DTS.
                .args([
                    "-map",
                    "0",
                    "-vsync",
                    "0",
                    "-enc_time_base",
                    "-1",
                    "-f",
                    "null",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                decoded.status.success() && decoded.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            probe
        }
    }

    impl Drop for TestRecording {
        fn drop(&mut self) {
            let _ = self.stop.send(true);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            let _ = std::fs::remove_file(&self.session.output_path);
        }
    }

    #[test]
    fn finalizes_playable_video_and_two_audio_tracks() {
        let mut recording = TestRecording::start(2);
        std::thread::sleep(Duration::from_millis(600));
        recording.finish().unwrap();
        let probe = recording.probe();
        let streams = probe["streams"].as_array().unwrap();
        assert_eq!(
            streams.iter().filter(|s| s["codec_name"] == "h264").count(),
            1
        );
        assert_eq!(
            streams.iter().filter(|s| s["codec_name"] == "aac").count(),
            2
        );
        assert!(
            probe["format"]["duration"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
                > 0.4
        );
    }

    #[test]
    fn pause_resume_omits_paused_time_and_stop_while_paused_finalizes() {
        let mut recording = TestRecording::start(0);
        std::thread::sleep(Duration::from_millis(400));
        recording.control(true);
        let before = recording
            .pipeline
            .query_position::<gst::ClockTime>()
            .unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let after = recording
            .pipeline
            .query_position::<gst::ClockTime>()
            .unwrap();
        assert!(after.saturating_sub(before) < gst::ClockTime::from_mseconds(100));
        recording.control(false);
        std::thread::sleep(Duration::from_millis(400));
        recording.control(true);
        let active_duration = recording
            .pipeline
            .query_position::<gst::ClockTime>()
            .unwrap()
            .nseconds() as f64
            / 1_000_000_000.0;
        recording.finish().unwrap();
        let probe = recording.probe();
        let duration = probe["format"]["duration"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap();
        assert!(
            (duration - active_duration).abs() < 0.2,
            "movie duration {duration}, active timeline {active_duration}"
        );
    }

    #[test]
    fn pipeline_errors_fail_the_recording() {
        let mut recording = TestRecording::start(0);
        gst::element_error!(
            recording.pipeline,
            gst::StreamError::Failed,
            ("injected capture failure")
        );
        let error = recording
            .worker
            .take()
            .unwrap()
            .join()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("injected capture failure"));
    }

    #[test]
    fn rejects_cpu_buffers_before_pixel_processing() {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch("videotestsrc num-buffers=1 name=source ! video/x-raw,width=320,height=180 ! fakesink").unwrap().downcast::<gst::Pipeline>().unwrap());
        let output = element("capsfilter").unwrap();
        attach_capture_probe(
            &pipeline.0.by_name("source").unwrap(),
            &output,
            Resolution::Native,
            Mode {
                kind: encoding::Kind::Va,
                factory: "vah264enc",
                dmabuf: true,
            },
            Arc::new(Counters::default()),
        );
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Error])
            .unwrap();
        let gst::MessageView::Error(error) = message.view() else {
            panic!("expected error")
        };
        assert!(error.error().to_string().contains("non-DMA-BUF"));
    }
}
