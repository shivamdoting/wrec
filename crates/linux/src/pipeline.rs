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

fn capture_caps(dmabuf: bool, pipewire: bool, fps: u32) -> gst::Caps {
    let caps = gst::Caps::builder("video/x-raw");
    // Wayland compositors advertise variable-rate frames (framerate=0/1).
    // Bound their maximum rate without requiring a fixed-rate source.
    let caps = if pipewire {
        caps.field(
            "max-framerate",
            gst::List::new(
                (1..=fps as i32)
                    .rev()
                    .map(|rate| gst::Fraction::new(rate, 1)),
            ),
        )
    } else {
        caps.field("framerate", gst::Fraction::new(fps as i32, 1))
    };
    if dmabuf {
        caps.features(["memory:DMABuf"])
            .field("format", "DMA_DRM")
            .build()
    } else {
        caps.build()
    }
}

fn movie_mux() -> Result<gst::Element> {
    let mux = element("qtmux")?;
    mux.set_property("fragment-duration", 10000u32);
    mux.set_property_from_str("fragment-mode", "first-moov-then-finalise");
    // Preserve sub-frame timestamps around pause/resume instead of rounding
    // them to the default frame-rate-derived track timescale.
    mux.set_property("trak-timescale", 1_000_000u32);
    Ok(mux)
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
    timeline: Mutex<Timeline>,
}

#[derive(Default)]
struct Timeline {
    paused_at: Option<gst::ClockTime>,
    offset: gst::ClockTime,
    accept_from: gst::ClockTime,
}

impl Timeline {
    fn pause(&mut self, now: gst::ClockTime) {
        self.paused_at.get_or_insert(now);
    }

    fn resume(&mut self, now: gst::ClockTime) {
        if let Some(paused_at) = self.paused_at.take() {
            self.offset += now.saturating_sub(paused_at);
            self.accept_from = now;
        }
    }

    fn position(&self, now: gst::ClockTime) -> gst::ClockTime {
        self.paused_at.unwrap_or(now).saturating_sub(self.offset)
    }

    fn retime(&self, buffer: &mut gst::Buffer) -> bool {
        if self.paused_at.is_some() || buffer.pts().is_some_and(|pts| pts < self.accept_from) {
            return false;
        }
        if self.offset > gst::ClockTime::ZERO {
            // make_mut copies a shared buffer header, retaining references to
            // its memory. DMA-BUF pixels remain on the GPU.
            let buffer = buffer.make_mut();
            buffer.set_pts(buffer.pts().map(|pts| pts.saturating_sub(self.offset)));
            buffer.set_dts(buffer.dts().map(|dts| dts.saturating_sub(self.offset)));
        }
        true
    }
}

pub(crate) enum CaptureInput {
    PipeWire(Box<dyn Fn() -> Result<PipeWireStream> + Send>),
    X11 { display: String, xid: u64 },
}

struct Attempt<'a> {
    mode: Mode,
    counters: Arc<Counters>,
    last: bool,
    stopping: &'a dyn Fn(),
}

fn finished(result: &Result<()>, counters: &Counters) -> bool {
    matches!(result, Ok(()) | Err(RecorderError::Cancelled))
        || counters.frames.load(Ordering::Relaxed) > 0
}

fn ends_recording(result: &Result<()>, counters: &Counters, last: bool) -> bool {
    last || finished(result, counters)
}

pub(crate) fn record(
    capture: &CaptureInput,
    session: &RecordingSession,
    settings: &RecorderSettings,
    events: &mpsc::Sender<RecorderEvent>,
    commands: mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    stopping: &dyn Fn(),
) -> Result<()> {
    if matches!(capture, CaptureInput::X11 { .. }) {
        crate::x11::initialize()?;
    }
    check_plugins(settings)?;
    let dmabuf = matches!(capture, CaptureInput::PipeWire(_)) && encoding::va_imports_dma_buf();
    let modes = encoding::modes(settings.codec, dmabuf);
    if modes.is_empty() {
        return Err(backend(format!("No {} encoder is installed. Install GStreamer VA/NVENC plugins or x264/x265/openh264 for software compatibility; see packaging/linux/README.md.", settings.codec.as_arg())));
    }
    let mut last_error = None;
    let count = modes.len();
    for (index, mode) in modes.into_iter().enumerate() {
        if *stop.borrow() {
            return Err(RecorderError::Cancelled);
        }
        let attempt = Attempt {
            mode,
            counters: Arc::new(Counters::default()),
            last: index + 1 == count,
            stopping,
        };
        let _ = events.send(RecorderEvent::Log {
            session_id: Some(session.id),
            message: format!("capture-engine: trying {}", mode.description()),
        });
        let result = record_attempt(
            capture, session, settings, events, &commands, stop, &attempt,
        );
        release_freed_memory();
        if finished(&result, &attempt.counters) {
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

#[cfg(target_env = "gnu")]
fn release_freed_memory() {
    extern "C" {
        fn malloc_trim(pad: usize) -> std::os::raw::c_int;
    }
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(target_env = "gnu"))]
fn release_freed_memory() {}

fn record_attempt(
    capture: &CaptureInput,
    session: &RecordingSession,
    settings: &RecorderSettings,
    events: &mpsc::Sender<RecorderEvent>,
    commands: &mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    attempt: &Attempt<'_>,
) -> Result<()> {
    if *stop.borrow() {
        return Err(RecorderError::Cancelled);
    }
    let settings = settings.clone().with_preset_limits();
    let stream = match capture {
        CaptureInput::PipeWire(connect) => Some(connect()?),
        CaptureInput::X11 { .. } => None,
    };
    let source_watch = match capture {
        CaptureInput::X11 { xid, .. } if *xid != 0 => Some(crate::x11::SourceWatch::new(*xid)?),
        _ => None,
    };
    let pipeline = PipelineGuard(gst::Pipeline::new());
    pipeline.0.use_clock(Some(&gst::SystemClock::obtain()));
    let mode = attempt.mode;
    let source = match capture {
        CaptureInput::PipeWire(_) => {
            let capture = stream.as_ref().unwrap();
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
    input.set_property(
        "caps",
        capture_caps(
            mode.dmabuf,
            matches!(capture, CaptureInput::PipeWire(_)),
            settings.fps.as_u32(),
        ),
    );
    let queue = video_queue()?;
    let converters = mode.converters()?;
    let output = element("capsfilter")?;
    output.set_property("caps", mode.caps(None));
    let encoder = mode.encoder(&settings)?;
    let parser = element(parser_name(settings.codec))?;
    let mux = movie_mux()?;
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
    let counters = attempt.counters.clone();
    if settings.include_system_audio {
        add_audio(&pipeline.0, &mux, Some("@DEFAULT_MONITOR@"), &counters)?;
    }
    if settings.include_microphone {
        add_audio(&pipeline.0, &mux, None, &counters)?;
    }
    attach_capture_probe(
        &source,
        &output,
        settings.resolution,
        mode,
        counters.clone(),
    );
    keep_source_configuration(&queue);
    if rewrites_timestamps(mode, &encoder) {
        keep_capture_timestamps(&encoder, &parser);
    }
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
    let result = run(
        &pipeline.0,
        session,
        events,
        commands,
        stop,
        &counters,
        &|| match (&stream, &source_watch) {
            (Some(stream), _) => stream.lost(),
            (None, Some(watch)) => watch.lost().then_some(SOURCE_LOST),
            (None, None) => None,
        },
    );
    if ends_recording(&result, &counters, attempt.last) {
        (attempt.stopping)();
    }
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

const CAPTURE_TIME: &str = "timestamp/x-wrec-capture";

fn rewrites_timestamps(mode: Mode, encoder: &gst::Element) -> bool {
    mode.kind == encoding::Kind::Va
        && encoder
            .factory()
            .and_then(|factory| encoding::plugin_version(&factory))
            .is_some_and(|version| version < vec![1, 24, 3])
}

fn segment(pad: &gst::Pad) -> Option<gst::FormattedSegment<gst::ClockTime>> {
    pad.sticky_event::<gst::event::Segment>(0)
        .and_then(|event| event.segment().clone().downcast::<gst::ClockTime>().ok())
}

const LOST_CAPTURE_TIME: &str = "A frame lost its capture timestamp in a VA encoder that replaces timestamps (GStreamer VA before 1.24.3). The recording stopped instead of writing video with incorrect timing.";

fn lose_capture_time(pad: &gst::Pad, info: &mut gst::PadProbeInfo<'_>) -> gst::PadProbeReturn {
    if let Some(element) = pad.parent_element() {
        gst::element_error!(element, gst::StreamError::Format, ("{}", LOST_CAPTURE_TIME));
    }
    info.flow_res = Err(gst::FlowError::Error);
    gst::PadProbeReturn::Handled
}

fn keep_capture_timestamps(encoder: &gst::Element, parser: &gst::Element) {
    let reference = gst::Caps::new_empty_simple(CAPTURE_TIME);
    encoder
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let running_time = info
                .buffer()
                .and_then(|buffer| buffer.pts())
                .zip(segment(pad))
                .and_then(|(pts, segment)| segment.to_running_time(pts));
            if let (Some(running_time), Some(gst::PadProbeData::Buffer(buffer))) =
                (running_time, info.data.as_mut())
            {
                gst::ReferenceTimestampMeta::add(
                    buffer.make_mut(),
                    &reference,
                    running_time,
                    gst::ClockTime::NONE,
                );
                return gst::PadProbeReturn::Ok;
            }
            lose_capture_time(pad, info)
        });
    parser
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let pts = info
                .buffer()
                .and_then(|buffer| {
                    buffer
                        .iter_meta::<gst::ReferenceTimestampMeta>()
                        .find(|meta| {
                            meta.reference()
                                .structure(0)
                                .is_some_and(|structure| structure.name() == CAPTURE_TIME)
                        })
                        .map(|meta| meta.timestamp())
                })
                .zip(segment(pad))
                .and_then(|(running_time, segment)| {
                    segment.position_from_running_time(running_time)
                });
            if let (Some(pts), Some(gst::PadProbeData::Buffer(buffer))) = (pts, info.data.as_mut())
            {
                let buffer = buffer.make_mut();
                buffer.set_pts(pts);
                buffer.set_dts(pts);
                return gst::PadProbeReturn::Ok;
            }
            lose_capture_time(pad, info)
        });
}

fn add_audio(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    device: Option<&str>,
    counters: &Arc<Counters>,
) -> Result<()> {
    let source = element("pulsesrc")?;
    source.set_property("provide-clock", false);
    if let Some(device) = device {
        source.set_property("device", device);
    }
    add_audio_source(pipeline, mux, &source, counters)
}

fn add_audio_source(
    pipeline: &gst::Pipeline,
    mux: &gst::Element,
    source: &gst::Element,
    counters: &Arc<Counters>,
) -> Result<()> {
    attach_timing_probe(source, counters.clone());
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

fn retime(info: &mut gst::PadProbeInfo<'_>, counters: &Counters) -> bool {
    if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
        return counters.timeline.lock().unwrap().retime(buffer);
    }
    true
}

fn attach_timing_probe(source: &gst::Element, counters: Arc<Counters>) {
    source
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if retime(info, &counters) {
                gst::PadProbeReturn::Ok
            } else {
                gst::PadProbeReturn::Drop
            }
        });
}

thread_local! {
    static SETTING_CANVAS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn set_canvas(output: &gst::Element, caps: gst::Caps) {
    SETTING_CANVAS.with(|setting| setting.set(true));
    output.set_property("caps", caps);
    SETTING_CANVAS.with(|setting| setting.set(false));
}

fn keep_source_configuration(queue: &gst::Element) {
    queue
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::EVENT_UPSTREAM, |_, info| {
            let canvas = info
                .event()
                .is_some_and(|event| event.type_() == gst::EventType::Reconfigure)
                && SETTING_CANVAS.with(|setting| setting.get());
            if canvas {
                gst::PadProbeReturn::Handled
            } else {
                gst::PadProbeReturn::Ok
            }
        });
}

fn attach_capture_probe(
    source: &gst::Element,
    output: &gst::Element,
    resolution: Resolution,
    mode: Mode,
    counters: Arc<Counters>,
) {
    let output = output.clone();
    let resized = std::sync::atomic::AtomicBool::new(false);
    source.static_pad("src").unwrap().add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
        move |pad, info| {
            if !retime(info, &counters) {
                return gst::PadProbeReturn::Drop;
            }
            if let Some(gst::PadProbeData::Event(event)) = &info.data {
                if let gst::EventView::Caps(caps) = event.view() {
                    if let Some(structure) = caps.caps().structure(0) {
                        if let (Ok(width), Ok(height)) = (
                            structure.get::<i32>("width"),
                            structure.get::<i32>("height"),
                        ) {
                            if width >= 2 && height >= 2 {
                                let mut dimensions = counters.dimensions.lock().unwrap();
                                resized.store(dimensions.is_some(), Ordering::Relaxed);
                                // A MOV track has a fixed canvas. Window resizes are
                                // scaled/letterboxed into the initial output size.
                                let (w, h) = dimensions
                                    .map(|d| (d.output_width as i32, d.output_height as i32))
                                    .unwrap_or_else(|| output_size(width, height, resolution));
                                dimensions.get_or_insert(CaptureDimensions {
                                    native_width: width.into(),
                                    native_height: height.into(),
                                    output_width: w.into(),
                                    output_height: h.into(),
                                });
                                drop(dimensions);
                                set_canvas(&output, mode.caps(Some((w, h))));
                            }
                        }
                    }
                }
            }
            if let Some(gst::PadProbeData::Buffer(buffer)) = &info.data {
                if buffer.flags().contains(gst::BufferFlags::CORRUPTED)
                    && counters.last_pts.lock().unwrap().is_none()
                {
                    return reject_capture(
                        pad,
                        info,
                        gst::StreamError::Failed,
                        UNFINISHED_FIRST_FRAME,
                    );
                }
                if let Some(pts) = buffer.pts() {
                    let mut previous = counters.last_pts.lock().unwrap();
                    if previous.is_some_and(|last| {
                        pts <= last || pts.saturating_sub(last) < gst::ClockTime::from_useconds(1)
                    }) {
                        counters.dropped.fetch_add(1, Ordering::Relaxed);
                        return gst::PadProbeReturn::Drop;
                    }
                    *previous = Some(pts);
                }
                if mode.dmabuf
                    && (buffer.n_memory() == 0
                        || buffer.iter_memories().any(|memory| {
                            !memory.is_memory_type::<gstreamer_allocators::DmaBufMemory>()
                        }))
                {
                    return reject_capture(pad, info, gst::StreamError::Format, NON_DMA_BUF_FRAME);
                }
                if resized.swap(false, Ordering::Relaxed) {
                    let mut allocation =
                        gst::query::Allocation::new(pad.current_caps().as_ref(), true);
                    pad.peer_query(&mut allocation);
                }
            }
            gst::PadProbeReturn::Ok
        },
    );
}

fn state_error(pipeline: &gst::Pipeline, fallback: impl std::fmt::Display) -> RecorderError {
    if let Some(message) = pipeline.bus().and_then(|bus| {
        bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Error],
        )
    }) {
        if let gst::MessageView::Error(error) = message.view() {
            return backend(format!(
                "{}: {} ({})",
                error
                    .src()
                    .map(|src| src.name().to_string())
                    .unwrap_or_default(),
                error.error(),
                error.debug().unwrap_or_default()
            ));
        }
    }
    backend(fallback)
}

const NON_DMA_BUF_FRAME: &str = "PipeWire delivered a non-DMA-BUF frame. This attempt requires GPU buffer sharing; another available mode will be tried before capture starts.";

fn reject_capture(
    pad: &gst::Pad,
    info: &mut gst::PadProbeInfo<'_>,
    error: gst::StreamError,
    message: &str,
) -> gst::PadProbeReturn {
    if let Some(element) = pad.parent_element() {
        gst::element_error!(element, error, ("{}", message));
    }
    info.flow_res = Err(gst::FlowError::Error);
    gst::PadProbeReturn::Handled
}

const UNFINISHED_FIRST_FRAME: &str = "The screen-capture source started with an unfinished frame. Its timestamp would stall capture timing, so this attempt stopped before encoding and another available mode will be tried.";

const SOURCE_LOST: &str =
    "The captured X11 window closed, was minimized, or became unavailable; the recording stopped.";

fn run(
    pipeline: &gst::Pipeline,
    session: &RecordingSession,
    events: &mpsc::Sender<RecorderEvent>,
    commands: &mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    counters: &Counters,
    source_lost: &dyn Fn() -> Option<&'static str>,
) -> Result<()> {
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|error| state_error(pipeline, error))?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| backend("recording pipeline has no bus"))?;
    let launched = Instant::now();
    let mut started = false;
    let mut stopping = None;
    let mut last_metrics = Instant::now();
    loop {
        if let Some(message) = stopping.is_none().then(source_lost).flatten() {
            return Err(backend(message));
        }
        if *stop.borrow() && stopping.is_none() {
            if !started && counters.frames.load(Ordering::Relaxed) == 0 {
                return Err(RecorderError::Cancelled);
            }
            // Reopen the input gate so EOS can carry the final frame. Keeping
            // native sources PLAYING avoids PipeWire pause/resume renegotiation.
            counters
                .timeline
                .lock()
                .unwrap()
                .resume(pipeline.current_running_time().unwrap_or_default());
            if !pipeline.send_event(gst::event::Eos::new()) {
                return Err(backend("recording pipeline rejected finalization"));
            }
            stopping = Some(Instant::now());
        }
        if let Ok(command) = commands.try_recv() {
            let (pause, reply) = match command {
                Command::Pause(reply) => (true, reply),
                Command::Resume(reply) => (false, reply),
            };
            let result = if stopping.is_some() || !started {
                Err(backend("recording is not ready for pause/resume"))
            } else {
                let now = pipeline.current_running_time().unwrap_or_default();
                let mut timeline = counters.timeline.lock().unwrap();
                if pause {
                    timeline.pause(now);
                } else {
                    timeline.resume(now);
                }
                Ok(())
            };
            let _ = reply.send(result);
        }
        if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    if let Some(lost) = source_lost() {
                        return Err(backend(lost));
                    }
                    return Err(backend(format!(
                        "{}: {} ({})",
                        error
                            .src()
                            .map(|src| src.name().to_string())
                            .unwrap_or_default(),
                        error.error(),
                        error.debug().unwrap_or_default()
                    )));
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
    let elapsed_secs = counters
        .timeline
        .lock()
        .unwrap()
        .position(pipeline.current_running_time().unwrap_or_default())
        .seconds();
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
    fn accepts_variable_rate_portal_frames_and_caps_their_maximum() {
        gst::init().unwrap();
        let portal = gst::Caps::builder("video/x-raw")
            .features(["memory:DMABuf"])
            .field("framerate", gst::Fraction::new(0, 1))
            .field("max-framerate", gst::Fraction::new(30, 1))
            .build();
        assert!(capture_caps(true, true, 30).can_intersect(&portal));
        assert!(!capture_caps(true, false, 30).can_intersect(&portal));
        assert_eq!(
            capture_caps(true, true, 30)
                .structure(0)
                .unwrap()
                .get::<gst::List>("max-framerate")
                .unwrap()
                .first()
                .unwrap()
                .get::<gst::Fraction>()
                .unwrap(),
            gst::Fraction::new(30, 1)
        );
    }

    fn negotiated_ceiling(capture: &gst::Caps, portal: &str) -> Option<gst::Fraction> {
        let portal = portal.parse::<gst::Caps>().unwrap();
        let mut common = capture.intersect_with_mode(&portal, gst::CapsIntersectMode::First);
        if common.is_empty() {
            return None;
        }
        common.fixate();
        common
            .structure(0)
            .unwrap()
            .get::<gst::Fraction>("max-framerate")
            .ok()
    }

    #[test]
    fn portal_maximum_rates_below_the_request_become_the_ceiling() {
        gst::init().unwrap();
        for dmabuf in [false, true] {
            let features = if dmabuf {
                "(memory:DMABuf), format=DMA_DRM, drm-format=XR24:0x0200000000000901"
            } else {
                ", format=BGRx"
            };
            let portal = |maximum: &str| {
                format!("video/x-raw{features}, framerate=0/1, max-framerate={maximum}")
            };
            for (fps, maximum, ceiling) in [
                (30, "[1/1, 5/1]", 5),
                (30, "[1/1, 24/1]", 24),
                (30, "[1/1, 60/1]", 30),
                (30, "[1/1, 144/1]", 30),
                (60, "[1/1, 5/1]", 5),
                (60, "[1/1, 60000/1001]", 59),
                (60, "[1/1, 60/1]", 60),
                (60, "[1/1, 144/1]", 60),
            ] {
                assert_eq!(
                    negotiated_ceiling(&capture_caps(dmabuf, true, fps), &portal(maximum)),
                    Some(gst::Fraction::new(ceiling, 1)),
                    "dmabuf {dmabuf}, {fps} fps requested, portal maximum {maximum}"
                );
            }
            for (fps, maximum) in [(30, "60/1"), (30, "[40/1, 60/1]"), (60, "144/1")] {
                assert_eq!(
                    negotiated_ceiling(&capture_caps(dmabuf, true, fps), &portal(maximum)),
                    None,
                    "dmabuf {dmabuf}, {fps} fps requested, portal maximum {maximum}"
                );
            }
        }
    }

    #[test]
    fn dma_buf_capture_requires_an_explicit_drm_format() {
        gst::init().unwrap();
        let capture = capture_caps(true, true, 60);
        let caps = |description: &str| description.parse::<gst::Caps>().unwrap();
        let legacy_source = caps("video/x-raw(memory:DMABuf), format=BGRx, width=1280, height=720, framerate=0/1, max-framerate=60/1");
        let legacy_converter = caps("video/x-raw(memory:DMABuf), width=[1, 16384], height=[1, 16384], format={ BGRA, RGBA, BGRx, RGBx, NV12, P010_10LE }");
        let modern_source = caps("video/x-raw(memory:DMABuf), format=DMA_DRM, drm-format=XR24:0x0200000000000901, width=1280, height=720, framerate=0/1, max-framerate=60/1");
        let modern_converter = caps("video/x-raw(memory:DMABuf), width=[1, 16384], height=[1, 16384], format=DMA_DRM, drm-format={ NV12:0x0200000000000901, XR24:0x0200000000000901 }");
        assert!(!capture.can_intersect(&legacy_source));
        assert!(!capture.can_intersect(&legacy_converter));
        assert!(capture.can_intersect(&modern_source));
        assert!(capture.can_intersect(&modern_converter));
        assert!(capture_caps(false, true, 60).can_intersect(&caps(
            "video/x-raw, format=BGRx, width=1280, height=720, framerate=0/1, max-framerate=60/1"
        )));
    }

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
        counters: Arc<Counters>,
        lost: Arc<std::sync::atomic::AtomicBool>,
    }

    impl TestRecording {
        fn start(audio_tracks: usize) -> Self {
            Self::start_with_audio_delays(&vec![gst::ClockTime::ZERO; audio_tracks])
        }

        fn start_with_audio_delays(audio_delays: &[gst::ClockTime]) -> Self {
            Self::start_with(audio_delays, false)
        }

        fn start_with(audio_delays: &[gst::ClockTime], counting_encoder: bool) -> Self {
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
            let pipeline = gst::parse::launch("videotestsrc name=video is-live=true pattern=ball ! video/x-raw,width=320,height=180,framerate=30/1 ! openh264enc name=encoder ! h264parse name=parser")
                .unwrap().downcast::<gst::Pipeline>().unwrap();
            let mux = movie_mux().unwrap();
            let sink = element("filesink").unwrap();
            sink.set_property("sync", false);
            sink.set_property("location", session.output_path.to_str().unwrap());
            pipeline.add_many([&mux, &sink]).unwrap();
            gst::Element::link_many([&pipeline.by_name("parser").unwrap(), &mux, &sink]).unwrap();
            let counters = Arc::new(Counters::default());
            attach_timing_probe(&pipeline.by_name("video").unwrap(), counters.clone());
            let encoder = pipeline.by_name("encoder").unwrap();
            if counting_encoder {
                count_frames_like_old_va_encoders(&pipeline.by_name("video").unwrap(), &encoder);
            }
            keep_capture_timestamps(&encoder, &pipeline.by_name("parser").unwrap());
            for &delay in audio_delays {
                let source = element("audiotestsrc").unwrap();
                source.set_property("is-live", true);
                source.static_pad("src").unwrap().add_probe(
                    gst::PadProbeType::BUFFER,
                    move |_, info| match info.buffer().and_then(|buffer| buffer.pts()) {
                        Some(pts) if pts < delay => gst::PadProbeReturn::Drop,
                        _ => gst::PadProbeReturn::Ok,
                    },
                );
                add_audio_source(&pipeline, &mux, &source, &counters).unwrap();
            }
            attach_encoded_probe(&pipeline.by_name("parser").unwrap(), counters.clone());
            let (events_tx, events) = mpsc::channel();
            let (commands, commands_rx) = mpsc::sync_channel(1);
            let (stop, stopped) = watch::channel(false);
            let worker_pipeline = pipeline.clone();
            let worker_session = session.clone();
            let worker_counters = counters.clone();
            let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker_lost = lost.clone();
            let worker = std::thread::spawn(move || {
                let guard = PipelineGuard(worker_pipeline);
                run(
                    &guard.0,
                    &worker_session,
                    &events_tx,
                    &commands_rx,
                    &stopped,
                    &worker_counters,
                    &|| {
                        worker_lost
                            .load(Ordering::SeqCst)
                            .then_some(crate::portal::PIPEWIRE_LOST)
                    },
                )
            });
            let recording = Self {
                pipeline,
                session,
                events,
                commands,
                stop,
                worker: Some(worker),
                counters,
                lost,
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
            probe_movie(&self.session.output_path)
        }
    }

    fn count_frames_like_old_va_encoders(source: &gst::Element, encoder: &gst::Element) {
        source.static_pad("src").unwrap().add_probe(
            gst::PadProbeType::BUFFER,
            |_, info| match info.buffer().map(|buffer| buffer.offset() % 3) {
                Some(2) => gst::PadProbeReturn::Drop,
                _ => gst::PadProbeReturn::Ok,
            },
        );
        let start = gst::ClockTime::from_seconds(60 * 60 * 1000);
        let frame_duration = gst::ClockTime::SECOND / 30;
        let first_pts = Mutex::new(None);
        let count = AtomicU64::new(0);
        encoder.static_pad("src").unwrap().add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                match info.data.as_mut() {
                    Some(gst::PadProbeData::Event(event)) => {
                        if let gst::EventView::Segment(segment) = event.view() {
                            let mut segment = segment
                                .segment()
                                .clone()
                                .downcast::<gst::ClockTime>()
                                .unwrap();
                            segment.set_start(segment.start().map(|time| time + start));
                            segment.set_position(segment.position().map(|time| time + start));
                            segment.set_stop(segment.stop().map(|time| time + start));
                            *event = gst::event::Segment::new(&segment);
                        }
                    }
                    Some(gst::PadProbeData::Buffer(buffer)) => {
                        let first = *first_pts
                            .lock()
                            .unwrap()
                            .get_or_insert(buffer.pts().unwrap_or_default());
                        let pts =
                            start + first + frame_duration * count.fetch_add(1, Ordering::Relaxed);
                        let buffer = buffer.make_mut();
                        buffer.set_pts(pts);
                        buffer.set_dts(pts);
                        buffer.set_duration(frame_duration);
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            },
        );
    }

    fn probe_movie(movie: &Path) -> serde_json::Value {
        let probe = decode_movie(movie);
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
        probe
    }

    fn decode_movie(movie: &Path) -> serde_json::Value {
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
            .arg(movie)
            .output()
            .expect("install ffmpeg to run Linux recording tests");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let decoded = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(movie)
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
        serde_json::from_slice(&output.stdout).unwrap()
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

    fn start_seconds(probe: &serde_json::Value, codec: &str) -> Vec<f64> {
        probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|stream| stream["codec_name"] == codec)
            .map(|stream| stream["start_time"].as_str().unwrap().parse().unwrap())
            .collect()
    }

    fn assert_late_audio_keeps_its_offset(movie: &Path, probe: &serde_json::Value, delay: f64) {
        let video = start_seconds(probe, "h264");
        let audio = start_seconds(probe, "aac");
        assert_eq!((video.len(), audio.len()), (1, 2), "{}", movie.display());
        let (prompt, late) = (audio[0], audio[1]);
        assert!(
            (late - prompt - delay).abs() < 0.05,
            "{}: audio starts {prompt}s and {late}s, expected {delay}s apart",
            movie.display()
        );
        assert!(
            (late - video[0] - delay).abs() < 0.1,
            "{}: late audio starts {late}s, video {}s, expected {delay}s apart",
            movie.display(),
            video[0]
        );
    }

    #[test]
    fn late_audio_keeps_its_capture_timestamp_in_partial_and_finalized_movies() {
        let delay = gst::ClockTime::from_seconds(1);
        let mut recording = TestRecording::start_with_audio_delays(&[gst::ClockTime::ZERO, delay]);
        std::thread::sleep(Duration::from_secs(13));
        let partial = recording.session.output_path.with_extension("partial.mov");
        std::fs::copy(&recording.session.output_path, &partial).unwrap();
        recording.finish().unwrap();
        let seconds = delay.nseconds() as f64 / 1_000_000_000.0;
        let checks = std::panic::catch_unwind(|| {
            assert_late_audio_keeps_its_offset(&partial, &decode_movie(&partial), seconds);
            let movie = &recording.session.output_path;
            assert_late_audio_keeps_its_offset(movie, &probe_movie(movie), seconds);
        });
        let _ = std::fs::remove_file(&partial);
        if let Err(panic) = checks {
            std::panic::resume_unwind(panic);
        }
    }

    fn end_seconds(probe: &serde_json::Value, codec: &str) -> f64 {
        let stream = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_name"] == codec)
            .unwrap();
        let seconds = |key: &str| stream[key].as_str().unwrap().parse::<f64>().unwrap();
        seconds("start_time") + seconds("duration")
    }

    #[test]
    fn capture_timestamps_survive_encoders_that_count_frames() {
        let mut recording = TestRecording::start_with(&[gst::ClockTime::ZERO], true);
        std::thread::sleep(Duration::from_millis(1500));
        recording.control(true);
        std::thread::sleep(Duration::from_millis(700));
        recording.control(false);
        std::thread::sleep(Duration::from_millis(1500));
        let active = recording
            .counters
            .timeline
            .lock()
            .unwrap()
            .position(recording.pipeline.current_running_time().unwrap())
            .nseconds() as f64
            / 1_000_000_000.0;
        recording.finish().unwrap();
        let probe = recording.probe();
        let (video, audio) = (end_seconds(&probe, "h264"), end_seconds(&probe, "aac"));
        assert!(
            (video - active).abs() < 0.2,
            "video ends at {video}s after {active}s of active recording"
        );
        assert!(
            (video - audio).abs() < 0.15,
            "video ends at {video}s, audio at {audio}s"
        );
        let index = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_name"] == "h264")
            .unwrap()["index"]
            .clone();
        let mut pts: Vec<i64> = probe["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|packet| packet["stream_index"] == index)
            .map(|packet| packet["pts"].as_i64().unwrap())
            .collect();
        pts.sort_unstable();
        let skipped = pts
            .windows(2)
            .filter(|pair| pair[1] - pair[0] > 50_000)
            .count();
        assert!(
            skipped > 10,
            "{skipped} video gaps show dropped capture frames"
        );
    }

    #[test]
    fn lost_capture_timestamps_fail_instead_of_speeding_up_video() {
        let mut recording = TestRecording::start_with(&[gst::ClockTime::ZERO], true);
        std::thread::sleep(Duration::from_millis(500));
        recording
            .pipeline
            .by_name("encoder")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, |_, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
                    if let Some(meta) = buffer.make_mut().meta_mut::<gst::ReferenceTimestampMeta>()
                    {
                        meta.remove().unwrap();
                    }
                }
                gst::PadProbeReturn::Ok
            });
        std::thread::sleep(Duration::from_secs(1));
        let _ = recording.stop.send(true);
        let error = recording
            .worker
            .take()
            .unwrap()
            .join()
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains(LOST_CAPTURE_TIME), "{error}");
    }

    #[test]
    fn missing_capture_time_fails_the_push_with_a_stream_error() {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::Pipeline::new());
        let encoder = element("identity").unwrap();
        let parser = element("identity").unwrap();
        let sink = element("fakesink").unwrap();
        pipeline.0.add_many([&encoder, &parser, &sink]).unwrap();
        parser.link(&sink).unwrap();
        keep_capture_timestamps(&encoder, &parser);
        let _ = pipeline.0.set_state(gst::State::Playing);
        let source = gst::Pad::builder(gst::PadDirection::Src).build();
        source.set_active(true).unwrap();
        source.link(&parser.static_pad("sink").unwrap()).unwrap();
        assert!(source.push_event(gst::event::StreamStart::new("capture")));
        assert!(
            source.push_event(gst::event::Segment::new(&gst::FormattedSegment::<
                gst::ClockTime,
            >::new()))
        );
        let mut buffer = gst::Buffer::new();
        buffer
            .get_mut()
            .unwrap()
            .set_pts(gst::ClockTime::from_seconds(1));
        assert_eq!(source.push(buffer), Err(gst::FlowError::Error));
        let message = pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Error])
            .unwrap();
        let gst::MessageView::Error(error) = message.view() else {
            panic!("expected error")
        };
        assert!(
            error.error().to_string().contains(LOST_CAPTURE_TIME),
            "{}",
            error.error()
        );
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
        let mut recording = TestRecording::start(2);
        std::thread::sleep(Duration::from_millis(400));
        recording.control(true);
        std::thread::sleep(Duration::from_millis(100)); // Drain frames already accepted.
        let before = recording.counters.frames.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(600));
        let after = recording.counters.frames.load(Ordering::Relaxed);
        assert_eq!(after, before, "video frames must stop while paused");
        recording.control(false);
        std::thread::sleep(Duration::from_millis(400));
        recording.control(true);
        let active_duration = recording
            .counters
            .timeline
            .lock()
            .unwrap()
            .position(recording.pipeline.current_running_time().unwrap())
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
    fn every_attempt_that_ends_the_recording_signals_teardown() {
        let idle = Counters::default();
        let encoded = Counters::default();
        encoded.frames.store(1, Ordering::Relaxed);
        let failed = Err(backend("lost"));
        assert!(ends_recording(&Ok(()), &encoded, false));
        assert!(ends_recording(&Ok(()), &encoded, true));
        assert!(ends_recording(&Err(RecorderError::Cancelled), &idle, false));
        assert!(ends_recording(&failed, &encoded, false));
        assert!(ends_recording(&failed, &idle, true));
        assert!(!ends_recording(&failed, &idle, false));
    }

    #[test]
    fn source_loss_fails_active_and_paused_recordings_promptly() {
        for paused in [false, true] {
            let mut recording = TestRecording::start(1);
            std::thread::sleep(Duration::from_millis(300));
            if paused {
                recording.control(true);
            }
            let at = Instant::now();
            recording.lost.store(true, Ordering::SeqCst);
            let error = recording
                .worker
                .take()
                .unwrap()
                .join()
                .unwrap()
                .unwrap_err();
            assert!(at.elapsed() < Duration::from_secs(1), "{:?}", at.elapsed());
            assert_eq!(
                error.to_string(),
                format!("backend error: {}", crate::portal::PIPEWIRE_LOST)
            );
        }
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

    fn downstream_buffers(pipeline: &gst::Pipeline) -> Arc<AtomicU64> {
        let count = Arc::new(AtomicU64::new(0));
        let counted = count.clone();
        pipeline
            .by_name("sink")
            .unwrap()
            .static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                gst::PadProbeReturn::Ok
            });
        count
    }

    fn settle(pipeline: &gst::Pipeline) {
        pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(1), &[gst::MessageType::Eos]);
        let _ = pipeline.set_state(gst::State::Null);
    }

    fn first_unfinished_frame(corrupted_offset: u64) -> (Option<String>, u64) {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch("videotestsrc num-buffers=3 name=source ! video/x-raw,width=320,height=180 ! fakesink name=sink").unwrap().downcast::<gst::Pipeline>().unwrap());
        let delivered = downstream_buffers(&pipeline.0);
        let source = pipeline.0.by_name("source").unwrap();
        source
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
                    if buffer.offset() == corrupted_offset {
                        buffer.make_mut().set_flags(gst::BufferFlags::CORRUPTED);
                    }
                }
                gst::PadProbeReturn::Ok
            });
        attach_capture_probe(
            &source,
            &element("capsfilter").unwrap(),
            Resolution::Native,
            Mode {
                kind: encoding::Kind::Software,
                factory: "x264enc",
                dmabuf: false,
            },
            Arc::new(Counters::default()),
        );
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            )
            .unwrap();
        let error = match message.view() {
            gst::MessageView::Error(error) => Some(error.error().to_string()),
            _ => None,
        };
        settle(&pipeline.0);
        (error, delivered.load(Ordering::SeqCst))
    }

    #[test]
    fn an_unfinished_first_frame_fails_the_attempt_before_encoding() {
        let (error, delivered) = first_unfinished_frame(0);
        let error = error.expect("an unfinished first frame must fail");
        assert!(error.contains(UNFINISHED_FIRST_FRAME), "{error}");
        assert_eq!(delivered, 0, "no frame may follow a rejected first frame");
        assert_eq!(first_unfinished_frame(1), (None, 3));
    }

    fn source_reconfigures_while_scaling_to_720p(keep_source: bool) -> (u64, (i32, i32)) {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch("videotestsrc num-buffers=10 name=source ! video/x-raw,width=1920,height=1080,framerate=30/1 ! queue name=queue ! videoconvert ! videoscale add-borders=true ! capsfilter name=output ! fakesink name=sink").unwrap().downcast::<gst::Pipeline>().unwrap());
        let source = pipeline.0.by_name("source").unwrap();
        let output = pipeline.0.by_name("output").unwrap();
        let mode = Mode {
            kind: encoding::Kind::Software,
            factory: "x264enc",
            dmabuf: false,
        };
        output.set_property("caps", mode.caps(None));
        let configured = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reconfigures = Arc::new(AtomicU64::new(0));
        let (seen, counted) = (configured.clone(), reconfigures.clone());
        source.static_pad("src").unwrap().add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_UPSTREAM,
            move |_, info| {
                match info.event().map(|event| event.type_()) {
                    Some(gst::EventType::Caps) => seen.store(true, Ordering::SeqCst),
                    Some(gst::EventType::Reconfigure) if seen.load(Ordering::SeqCst) => {
                        counted.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                gst::PadProbeReturn::Ok
            },
        );
        attach_capture_probe(
            &source,
            &output,
            Resolution::R720p,
            mode,
            Arc::new(Counters::default()),
        );
        if keep_source {
            keep_source_configuration(&pipeline.0.by_name("queue").unwrap());
        }
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            )
            .unwrap();
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
        let caps = pipeline
            .0
            .by_name("sink")
            .unwrap()
            .static_pad("sink")
            .unwrap()
            .current_caps()
            .unwrap();
        let structure = caps.structure(0).unwrap();
        (
            reconfigures.load(Ordering::SeqCst),
            (
                structure.get::<i32>("width").unwrap(),
                structure.get::<i32>("height").unwrap(),
            ),
        )
    }

    #[test]
    fn setting_the_output_canvas_does_not_renegotiate_the_source() {
        assert_eq!(
            source_reconfigures_while_scaling_to_720p(true),
            (0, (1280, 720))
        );
        let (reconfigures, canvas) = source_reconfigures_while_scaling_to_720p(false);
        assert!(reconfigures > 0);
        assert_eq!(canvas, (1280, 720));
    }

    fn resize_without_source_allocation(
        converter: &str,
    ) -> (
        Vec<std::result::Result<gst::FlowSuccess, gst::FlowError>>,
        u64,
        u64,
    ) {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch(&format!("queue name=source ! queue name=queue ! {converter} ! capsfilter name=output ! fakesink name=sink")).unwrap().downcast::<gst::Pipeline>().unwrap());
        let delivered = downstream_buffers(&pipeline.0);
        let allocations = Arc::new(AtomicU64::new(0));
        let counted = allocations.clone();
        pipeline
            .0
            .by_name("queue")
            .unwrap()
            .static_pad("sink")
            .unwrap()
            .add_probe(
                gst::PadProbeType::QUERY_DOWNSTREAM | gst::PadProbeType::PUSH,
                move |_, info| {
                    if info
                        .query()
                        .is_some_and(|query| matches!(query.view(), gst::QueryView::Allocation(_)))
                    {
                        counted.fetch_add(1, Ordering::SeqCst);
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        let mode = Mode {
            kind: if converter == "vapostproc" {
                encoding::Kind::Va
            } else {
                encoding::Kind::Software
            },
            factory: "x264enc",
            dmabuf: false,
        };
        let output = pipeline.0.by_name("output").unwrap();
        output.set_property("caps", mode.caps(None));
        attach_capture_probe(
            &pipeline.0.by_name("source").unwrap(),
            &output,
            Resolution::Native,
            mode,
            Arc::new(Counters::default()),
        );
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let source = gst::Pad::builder(gst::PadDirection::Src).build();
        source.set_active(true).unwrap();
        source
            .link(
                &pipeline
                    .0
                    .by_name("source")
                    .unwrap()
                    .static_pad("sink")
                    .unwrap(),
            )
            .unwrap();
        assert!(source.push_event(gst::event::StreamStart::new("capture")));
        let mut results = Vec::new();
        let mut pts = gst::ClockTime::ZERO;
        for (index, (width, height)) in [(1920, 1080), (1920, 1080), (1280, 720), (1280, 720)]
            .into_iter()
            .enumerate()
        {
            if index == 0 || index == 2 {
                let caps =
                    format!("video/x-raw,format=RGBx,width={width},height={height},framerate=0/1")
                        .parse::<gst::Caps>()
                        .unwrap();
                assert!(source.push_event(gst::event::Caps::new(&caps)));
                if index == 0 {
                    assert!(source
                        .push_event(gst::event::Segment::new(&gst::FormattedSegment::<
                            gst::ClockTime,
                        >::new())));
                    source.peer_query(&mut gst::query::Allocation::new(Some(&caps), true));
                }
            }
            if index == 2 {
                allocations.store(0, Ordering::SeqCst);
            }
            let mut buffer = gst::Buffer::with_size((width * height * 4) as usize).unwrap();
            buffer.get_mut().unwrap().set_pts(pts);
            pts += gst::ClockTime::from_mseconds(33);
            results.push(source.push(buffer));
        }
        assert!(source.push_event(gst::event::Eos::new()));
        pipeline.0.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        (
            results,
            delivered.load(Ordering::SeqCst),
            allocations.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn a_source_resize_renegotiates_converter_allocation() {
        let (results, delivered, allocations) =
            resize_without_source_allocation("videoconvert ! videoscale add-borders=true");
        assert!(results.iter().all(|result| result.is_ok()), "{results:?}");
        assert_eq!((delivered, allocations), (4, 1));
        if gst::ElementFactory::find("vapostproc").is_some() {
            let (results, delivered, _) = resize_without_source_allocation("vapostproc");
            assert!(results.iter().all(|result| result.is_ok()), "{results:?}");
            assert_eq!(delivered, 4);
        }
    }

    #[test]
    fn rejects_cpu_buffers_before_pixel_processing() {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch("videotestsrc num-buffers=3 name=source ! video/x-raw,width=320,height=180 ! fakesink name=sink").unwrap().downcast::<gst::Pipeline>().unwrap());
        let delivered = downstream_buffers(&pipeline.0);
        pipeline
            .0
            .by_name("source")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, |_, info| {
                if let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_mut() {
                    if buffer.offset() >= 1 {
                        let size = buffer.size();
                        let path = std::env::temp_dir().join(format!(
                            "wrec-dmabuf-{}-{}",
                            std::process::id(),
                            buffer.offset()
                        ));
                        let file = std::fs::File::options()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .unwrap();
                        std::fs::remove_file(&path).unwrap();
                        file.set_len(size as u64).unwrap();
                        let memory = unsafe {
                            gstreamer_allocators::DmaBufAllocator::new().alloc(file, size)
                        }
                        .unwrap();
                        buffer.make_mut().replace_all_memory(memory);
                    }
                }
                gst::PadProbeReturn::Ok
            });
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
        settle(&pipeline.0);
        assert_eq!(delivered.load(Ordering::SeqCst), 0);
    }
}
