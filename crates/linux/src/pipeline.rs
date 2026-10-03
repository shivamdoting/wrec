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
        atomic::{AtomicU64, AtomicU8, Ordering},
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

const CAPTURE_QUEUE: &str = "capture-queue";

fn video_queue() -> Result<gst::Element> {
    let queue = element("queue")?;
    queue.set_property("name", CAPTURE_QUEUE);
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
    queued: AtomicU64,
    dequeued: AtomicU64,
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
    X11 {
        display: String,
        xid: u64,
    },
    #[cfg(test)]
    Element(Box<dyn Fn() -> gst::Element + Send>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Teardown {
    Recording,
    AttemptStarted,
    AttemptFinished,
}

struct Attempt<'a> {
    mode: Mode,
    counters: Arc<Counters>,
    last: bool,
    stopping: &'a dyn Fn(Teardown),
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
    stopping: &dyn Fn(Teardown),
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
        if !ends_recording(&result, &attempt.counters, attempt.last) {
            stopping(Teardown::AttemptFinished);
        }
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
        _ => None,
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
        #[cfg(test)]
        CaptureInput::Element(source) => source(),
    };
    source.set_property("do-timestamp", true);
    let input = element("capsfilter")?;
    input.set_property(
        "caps",
        capture_caps(
            mode.dmabuf,
            !matches!(capture, CaptureInput::X11 { .. }),
            settings.fps.as_u32(),
        ),
    );
    let queue = video_queue()?;
    let converters = mode.converters()?;
    let output = element("capsfilter")?;
    output.set_property("caps", mode.caps(None));
    let encoder = mode.encoder(&settings)?;
    let parser = element(parser_name(settings.codec))?;
    parser.set_property("config-interval", -1i32);
    let format = movie_format(settings.codec, settings.fps.as_u32())?;
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
    chain.extend([&output, &encoder, &parser, &format]);
    pipeline
        .0
        .add_many(chain.iter().copied())
        .map_err(backend)?;
    pipeline.0.add_many([&mux, &sink]).map_err(backend)?;
    gst::Element::link_many(chain.iter().copied()).map_err(backend)?;
    mux.link(&sink).map_err(backend)?;
    let counters = attempt.counters.clone();
    let mut movie = Movie::new(
        &pipeline.0,
        &format,
        &mux,
        settings.include_system_audio || settings.include_microphone,
    )?;
    if settings.include_system_audio {
        add_audio(
            &mut movie,
            Some("@DEFAULT_MONITOR@"),
            "system audio",
            &counters,
        )?;
    }
    if settings.include_microphone {
        add_audio(&mut movie, None, "microphone", &counters)?;
    }
    attach_capture_probe(
        &source,
        &output,
        settings.resolution,
        mode,
        counters.clone(),
    );
    keep_source_configuration(&queue);
    if mode.kind == encoding::Kind::Va {
        match_keyframes_to_capture(&encoder, settings.fps.as_u32());
    }
    if rewrites_timestamps(mode, &encoder) {
        keep_capture_timestamps(&encoder, &parser);
    }
    attach_encoded_probe(&parser, counters.clone());
    count_dropped(&queue, counters.clone());
    let _ = events.send(RecorderEvent::Log {
        session_id: Some(session.id),
        message: format!(
            "capture-engine: selected {}; capture queue limited to 2 frames",
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
        &mut movie,
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
    let ending = ends_recording(&result, &counters, attempt.last);
    (attempt.stopping)(if ending {
        Teardown::Recording
    } else {
        Teardown::AttemptStarted
    });
    let _ = pipeline.0.set_state(gst::State::Null);
    if matches!(result, Err(RecorderError::Cancelled))
        || counters.frames.load(Ordering::Relaxed) == 0
    {
        let _ = std::fs::remove_file(&session.output_path);
    }
    result
}

// The queue emits "overrun" before it rechecks its level, so the encoder can
// take a frame meanwhile and nothing is discarded. Count the frames entering
// and leaving the queue instead; buffers are not modified, since PipeWire
// reuses a frame's memory once its source buffer is released.
fn count_dropped(queue: &gst::Element, counters: Arc<Counters>) {
    let entering = counters.clone();
    queue
        .static_pad("sink")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            entering.queued.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    queue
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            counters.dequeued.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
}

// Frames rejected at capture plus frames the queue discarded: those that
// entered it, never left, and are not waiting in it. Exact once the queue is
// empty; while frames flow, a frame in transit can shift it by one.
fn dropped_frames(pipeline: &gst::Pipeline, counters: &Counters) -> u64 {
    let waiting = pipeline.by_name(CAPTURE_QUEUE).map_or(0, |queue| {
        queue.property::<u32>("current-level-buffers") as u64
    });
    let dequeued = counters.dequeued.load(Ordering::Relaxed);
    let queued = counters.queued.load(Ordering::Relaxed);
    counters.dropped.load(Ordering::Relaxed) + queued.saturating_sub(dequeued + waiting)
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

// Encoded video held while the movie waits for audio to start. Past either
// limit, the movie starts without the audio tracks that have not delivered a
// buffer. Paused time does not count, since no video arrives while paused.
// Constant-QP output has no fixed bitrate, so bytes are bounded separately.
const HELD_VIDEO_LIMIT: gst::ClockTime = gst::ClockTime::from_seconds(3);
const HELD_VIDEO_BYTES: u32 = 32 << 20;
const HELD_QUEUE: &str = "held-video-queue";

const WAITING: u8 = 0;
const STARTED: u8 = 1;
const ENDED: u8 = 2;
const OMITTED: u8 = 3;

struct AudioTrack {
    name: &'static str,
    pad: gst::Pad,
    probe: Option<gst::PadProbeId>,
    state: Arc<AtomicU8>,
}

// The recording pipeline and the tracks of its movie. qtmux writes a track
// for every pad requested from it, and a track that never received audio has
// no sample description, which makes the whole movie unreadable. AAC encoders
// only learn their format from their first input buffer, so audio pads are
// requested once each track has delivered a buffer or ended without one.
// Until then video waits in a queue instead of being dropped at capture.
struct Movie {
    pipeline: gst::Pipeline,
    mux: gst::Element,
    held: Option<(gst::Element, gst::PadProbeId)>,
    draining: Option<gst::Element>,
    audio: Vec<AudioTrack>,
}

impl Movie {
    fn new(
        pipeline: &gst::Pipeline,
        video: &gst::Element,
        mux: &gst::Element,
        audio: bool,
    ) -> Result<Self> {
        let mut movie = Self {
            pipeline: pipeline.clone(),
            mux: mux.clone(),
            held: None,
            draining: None,
            audio: Vec::new(),
        };
        let track = mux
            .request_pad_simple("video_%u")
            .ok_or_else(|| backend("the movie muxer refused a video track"))?;
        if !audio {
            video
                .static_pad("src")
                .unwrap()
                .link(&track)
                .map_err(backend)?;
            return Ok(movie);
        }
        // Headroom past the limits covers the run loop's 100 ms polling, so
        // held video is not pushed back to the capture queue before then.
        let queue = element("queue")?;
        queue.set_property("name", HELD_QUEUE);
        queue.set_property(
            "max-size-time",
            (HELD_VIDEO_LIMIT + gst::ClockTime::SECOND).nseconds(),
        );
        queue.set_property("max-size-buffers", 0u32);
        queue.set_property("max-size-bytes", HELD_VIDEO_BYTES + (8 << 20));
        pipeline.add(&queue).map_err(backend)?;
        video.link(&queue).map_err(backend)?;
        queue
            .static_pad("src")
            .unwrap()
            .link(&track)
            .map_err(backend)?;
        let probe = queue
            .static_pad("src")
            .unwrap()
            .add_probe(
                gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
                |_, _| gst::PadProbeReturn::Ok,
            )
            .unwrap();
        movie.held = Some((queue, probe));
        Ok(movie)
    }

    fn add_audio(&mut self, name: &'static str, queue: &gst::Element) {
        let pad = queue.static_pad("src").unwrap();
        // Linking the track sends a reconfigure upstream. The encoder answers
        // with an allocation query, which waits behind this queue while qtmux
        // holds audio for the next video frame, and pulsesrc loses audio
        // meanwhile. The audio caps are fixed, so nothing needs it.
        pad.add_probe(gst::PadProbeType::EVENT_UPSTREAM, |_, info| {
            if info
                .event()
                .is_some_and(|event| event.type_() == gst::EventType::Reconfigure)
            {
                gst::PadProbeReturn::Handled
            } else {
                gst::PadProbeReturn::Ok
            }
        });
        let state = Arc::new(AtomicU8::new(WAITING));
        let track = state.clone();
        let probe = pad.add_probe(
            gst::PadProbeType::BLOCK
                | gst::PadProbeType::BUFFER
                | gst::PadProbeType::EVENT_DOWNSTREAM,
            move |_, info| {
                let next = match &info.data {
                    Some(gst::PadProbeData::Buffer(_)) => STARTED,
                    Some(gst::PadProbeData::Event(event))
                        if event.type_() == gst::EventType::Eos =>
                    {
                        ENDED
                    }
                    _ => return gst::PadProbeReturn::Pass,
                };
                let _ = track.compare_exchange(WAITING, next, Ordering::SeqCst, Ordering::SeqCst);
                if next == ENDED {
                    return gst::PadProbeReturn::Pass;
                }
                if track.load(Ordering::SeqCst) == OMITTED {
                    gst::PadProbeReturn::Drop
                } else {
                    gst::PadProbeReturn::Ok
                }
            },
        );
        self.audio.push(AudioTrack {
            name,
            pad,
            probe,
            state,
        });
    }

    // Called on every pass of the run loop. Requests the audio pads once every
    // track has started or ended, or the held video reached its limit, then
    // releases the video. Returns the omitted tracks, each with whether it
    // ended before delivering audio.
    fn link_audio(&mut self) -> Result<Vec<(&'static str, bool)>> {
        if let Some(queue) = &self.draining {
            // Once the video held at startup has reached qtmux, a stalled
            // track holds video in qtmux and the capture queue drops frames,
            // as without this queue. Limiting it earlier would block the
            // encoder while the held video drains.
            if queue.property::<u32>("current-level-buffers") <= 1 {
                queue.set_property("max-size-time", 0u64);
                queue.set_property("max-size-bytes", 0u32);
                queue.set_property("max-size-buffers", 1u32);
                self.draining = None;
            }
        }
        let Some((queue, _)) = &self.held else {
            return Ok(Vec::new());
        };
        let full = queue.property::<u64>("current-level-time") >= HELD_VIDEO_LIMIT.nseconds()
            || queue.property::<u32>("current-level-bytes") >= HELD_VIDEO_BYTES;
        if !full
            && self
                .audio
                .iter()
                .any(|track| track.state.load(Ordering::SeqCst) == WAITING)
        {
            return Ok(Vec::new());
        }
        let mut omitted = Vec::new();
        for track in &mut self.audio {
            let state = track.state.load(Ordering::SeqCst);
            if state != STARTED
                && track
                    .state
                    .compare_exchange(state, OMITTED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                omitted.push((track.name, state == ENDED));
                continue;
            }
            let pad = self
                .mux
                .request_pad_simple("audio_%u")
                .ok_or_else(|| backend("the movie muxer refused an audio track"))?;
            track.pad.link(&pad).map_err(backend)?;
            if let Some(probe) = track.probe.take() {
                track.pad.remove_probe(probe);
            }
        }
        let (queue, probe) = self.held.take().unwrap();
        queue.static_pad("src").unwrap().remove_probe(probe);
        self.draining = Some(queue);
        Ok(omitted)
    }
}

fn add_audio(
    movie: &mut Movie,
    device: Option<&str>,
    name: &'static str,
    counters: &Arc<Counters>,
) -> Result<()> {
    let source = element("pulsesrc")?;
    source.set_property("provide-clock", false);
    if let Some(device) = device {
        source.set_property("device", device);
    }
    add_audio_source(movie, &source, name, counters)
}

fn add_audio_source(
    movie: &mut Movie,
    source: &gst::Element,
    name: &'static str,
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
    // Holds audio while the movie waits for video, or for another audio
    // track to start, which can take HELD_VIDEO_LIMIT.
    queue.set_property(
        "max-size-time",
        (HELD_VIDEO_LIMIT + gst::ClockTime::SECOND).nseconds(),
    );
    queue.set_property("max-size-buffers", 0u32);
    queue.set_property("max-size-bytes", 0u32);
    let chain = [
        source, &convert, &resample, &caps, &encoder, &parser, &queue,
    ];
    movie.pipeline.add_many(chain).map_err(backend)?;
    gst::Element::link_many(chain).map_err(backend)?;
    movie.add_audio(name, &queue);
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

// A fixed frame rate, else the maximum of a variable-rate stream.
fn capture_rate(caps: &gst::CapsRef) -> Option<gst::Fraction> {
    caps.structure(0).and_then(|structure| {
        ["framerate", "max-framerate"]
            .into_iter()
            .filter_map(|field| structure.get::<gst::Fraction>(field).ok())
            .find(|rate| rate.numer() > 0 && rate.denom() > 0)
    })
}

fn keyframe_interval(caps: &gst::CapsRef, fps: u32) -> u32 {
    let frames = capture_rate(caps).map_or(2 * fps as u64, |rate| {
        (2 * rate.numer() as u64).div_ceil(rate.denom() as u64)
    });
    frames.clamp(1, 2 * fps as u64) as u32
}

// VA encoders read key-int-max only when new caps reconfigure them, so set it
// as each CAPS event enters the encoder, after every frame of the old rate.
fn match_keyframes_to_capture(encoder: &gst::Element, fps: u32) {
    encoder.static_pad("sink").unwrap().add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM,
        move |pad, info| {
            if let (Some(gst::EventView::Caps(caps)), Some(encoder)) =
                (info.event().map(|event| event.view()), pad.parent_element())
            {
                let frames = keyframe_interval(caps.caps(), fps);
                if encoder.property::<u32>("key-int-max") != frames {
                    encoder.set_property("key-int-max", frames);
                }
            }
            gst::PadProbeReturn::Ok
        },
    );
}

// A capture rate change reconfigures the encoder, which writes new parameter
// sets. qtmux stores those as an extra sample description, which a fragmented
// movie cannot reference, so the movie keeps its first description and every
// keyframe carries its own parameter sets (avc3/hev1).
fn movie_format(codec: Codec, fps: u32) -> Result<gst::Element> {
    let format = element("capsfilter")?;
    format.set_property(
        "caps",
        match codec {
            Codec::H264 => gst::Caps::builder("video/x-h264").field("stream-format", "avc3"),
            Codec::Hevc => gst::Caps::builder("video/x-h265").field("stream-format", "hev1"),
        }
        .field("alignment", "au")
        .build(),
    );
    // qtmux times each sample by the next one and takes the last sample's
    // duration from its buffer. Some encoders (x265enc 1.24) leave it unset
    // for variable-rate capture, and players skip a zero-length last frame,
    // so a frame without one lasts a frame at the current capture rate, or at
    // the requested rate when the caps carry none.
    let requested = gst::ClockTime::SECOND / fps as u64;
    let first = Mutex::new(None::<gst::Event>);
    let frame = Mutex::new(requested);
    format.static_pad("src").unwrap().add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
        move |_, info| {
            match &mut info.data {
                Some(gst::PadProbeData::Event(event)) => {
                    if let gst::EventView::Caps(caps) = event.view() {
                        *frame.lock().unwrap() = capture_rate(caps.caps())
                            .and_then(|rate| {
                                gst::ClockTime::SECOND
                                    .mul_div_floor(rate.denom() as u64, rate.numer() as u64)
                            })
                            .unwrap_or(requested);
                        *event = first.lock().unwrap().get_or_insert(event.clone()).clone();
                    }
                }
                Some(gst::PadProbeData::Buffer(buffer)) if buffer.duration().is_none() => {
                    let duration = *frame.lock().unwrap();
                    buffer.make_mut().set_duration(duration);
                }
                _ => {}
            }
            gst::PadProbeReturn::Ok
        },
    );
    Ok(format)
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
    movie: &mut Movie,
    session: &RecordingSession,
    events: &mpsc::Sender<RecorderEvent>,
    commands: &mpsc::Receiver<Command>,
    stop: &watch::Receiver<bool>,
    counters: &Counters,
    source_lost: &dyn Fn() -> Option<&'static str>,
) -> Result<()> {
    let pipeline = &movie.pipeline.clone();
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
        for (name, ended) in movie.link_audio()? {
            let _ = events.send(RecorderEvent::Log {
                session_id: Some(session.id),
                message: format!(
                    "capture-engine: {name} sent no samples {}; the movie has no {name} track",
                    if ended {
                        "before the recording stopped".to_string()
                    } else {
                        format!(
                            "while {} seconds or {} MiB of video waited for it",
                            HELD_VIDEO_LIMIT.seconds(),
                            HELD_VIDEO_BYTES >> 20
                        )
                    }
                ),
            });
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
            dropped_frames: Some(dropped_frames(pipeline, counters)),
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
    fn keyframes_follow_the_negotiated_capture_rate_within_the_request() {
        gst::init().unwrap();
        let interval =
            |caps: &str, fps| keyframe_interval(&caps.parse::<gst::Caps>().unwrap(), fps);
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=5/1", 60),
            10
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=5/1", 30),
            10
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=24/1", 30),
            48
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=30/1", 30),
            60
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=60/1", 60),
            120
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=60000/1001", 60),
            120
        );
        assert_eq!(
            interval("video/x-raw, framerate=0/1, max-framerate=144/1", 60),
            120
        );
        assert_eq!(interval("video/x-raw, framerate=30/1", 30), 60);
        assert_eq!(interval("video/x-raw, framerate=60/1", 60), 120);
        assert_eq!(
            interval("video/x-raw, framerate=24/1, max-framerate=60/1", 60),
            48
        );
        assert_eq!(interval("video/x-raw, framerate=0/1", 60), 120);
        assert_eq!(interval("video/x-raw", 30), 60);
    }

    fn keyframes_from_va_encoder(factory: &'static str, match_capture: bool) -> (u32, usize) {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::parse::launch("videotestsrc name=source num-buffers=25 ! video/x-raw,width=320,height=180,framerate=5/1 ! vapostproc ! video/x-raw(memory:VAMemory),format=NV12 ! identity name=encoder-input ! fakesink name=sink").unwrap().downcast::<gst::Pipeline>().unwrap());
        let mode = Mode {
            kind: encoding::Kind::Va,
            factory,
            dmabuf: false,
        };
        let settings = RecorderSettings {
            fps: domain::FrameRate::Fps60,
            codec: if factory.contains("265") {
                Codec::Hevc
            } else {
                Codec::H264
            },
            ..RecorderSettings::default()
        };
        let encoder = mode.encoder(&settings).unwrap();
        let parser = element(parser_name(settings.codec)).unwrap();
        let input = pipeline.0.by_name("encoder-input").unwrap();
        let sink = pipeline.0.by_name("sink").unwrap();
        input.unlink(&sink);
        pipeline.0.add_many([&encoder, &parser]).unwrap();
        gst::Element::link_many([&input, &encoder, &parser, &sink]).unwrap();
        if match_capture {
            match_keyframes_to_capture(&encoder, 60);
        }
        let keyframes = Arc::new(AtomicU64::new(0));
        let counted = keyframes.clone();
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if info
                    .buffer()
                    .is_some_and(|buffer| !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT))
                {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                gst::PadProbeReturn::Ok
            });
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(20),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .unwrap();
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
        (
            encoder.property::<u32>("key-int-max"),
            keyframes.load(Ordering::SeqCst) as usize,
        )
    }

    #[test]
    fn va_keyframes_are_two_seconds_apart_at_the_capture_rate() {
        gst::init().unwrap();
        for factory in ["vah264enc", "vah265enc"] {
            if gst::ElementFactory::find(factory).is_none()
                || gst::ElementFactory::find("vapostproc").is_none()
            {
                continue;
            }
            assert_eq!(
                keyframes_from_va_encoder(factory, true),
                (10, 3),
                "{factory}"
            );
        }
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
            let pipeline = gst::parse::launch("videotestsrc name=video is-live=true pattern=ball ! capsfilter name=capture caps=video/x-raw,width=320,height=180,framerate=30/1 openh264enc name=encoder ! h264parse name=parser")
                .unwrap().downcast::<gst::Pipeline>().unwrap();
            // Like production, frames the encoder cannot take are dropped
            // before encoding instead of stalling capture.
            let queue = video_queue().unwrap();
            pipeline.add(&queue).unwrap();
            gst::Element::link_many([
                &pipeline.by_name("capture").unwrap(),
                &queue,
                &pipeline.by_name("encoder").unwrap(),
            ])
            .unwrap();
            let mux = movie_mux().unwrap();
            let sink = element("filesink").unwrap();
            sink.set_property("sync", false);
            sink.set_property("location", session.output_path.to_str().unwrap());
            pipeline.add_many([&mux, &sink]).unwrap();
            mux.link(&sink).unwrap();
            let mut movie = Movie::new(
                &pipeline,
                &pipeline.by_name("parser").unwrap(),
                &mux,
                !audio_delays.is_empty(),
            )
            .unwrap();
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
                add_audio_source(&mut movie, &source, "test audio", &counters).unwrap();
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
                let _guard = PipelineGuard(worker_pipeline);
                run(
                    &mut movie,
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

        fn logs(&self) -> Vec<String> {
            self.events
                .try_iter()
                .filter_map(|event| match event {
                    RecorderEvent::Log { message, .. } => Some(message),
                    _ => None,
                })
                .collect()
        }

        fn active_seconds(&self) -> f64 {
            self.counters
                .timeline
                .lock()
                .unwrap()
                .position(self.pipeline.current_running_time().unwrap())
                .nseconds() as f64
                / 1_000_000_000.0
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

    // The largest gap between packets of a stream up to the given time.
    fn largest_gap(probe: &serde_json::Value, kind: &str, until: f64) -> f64 {
        let index = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_type"] == kind)
            .unwrap()["index"]
            .clone();
        let mut pts: Vec<f64> = probe["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|packet| packet["stream_index"] == index)
            .map(|packet| packet["pts_time"].as_str().unwrap().parse().unwrap())
            .filter(|pts| *pts <= until)
            .collect();
        pts.sort_by(f64::total_cmp);
        pts.windows(2)
            .map(|pair| pair[1] - pair[0])
            .fold(0.0, f64::max)
    }

    fn assert_late_audio_keeps_its_offset(movie: &Path, probe: &serde_json::Value, delay: f64) {
        let video = start_seconds(probe, "h264");
        let audio = start_seconds(probe, "aac");
        assert_eq!((video.len(), audio.len()), (1, 2), "{}", movie.display());
        let (prompt, late) = (audio[0], audio[1]);
        let gap = largest_gap(probe, "video", late + 0.5);
        assert!(
            gap < 0.1,
            "{}: video skips {gap}s while waiting for late audio",
            movie.display()
        );
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

    fn stream_counts(probe: &serde_json::Value) -> (usize, usize) {
        let streams = probe["streams"].as_array().unwrap();
        let count = |codec| streams.iter().filter(|s| s["codec_name"] == codec).count();
        (count("h264"), count("aac"))
    }

    #[test]
    fn stopping_before_audio_arrives_keeps_the_captured_video() {
        let never = gst::ClockTime::MAX;
        for (delays, paused) in [
            (vec![never], false),
            (vec![never], true),
            (vec![gst::ClockTime::ZERO, never], false),
            (vec![gst::ClockTime::ZERO, never], true),
        ] {
            let case = format!("audio delays {delays:?}, paused {paused}");
            let mut recording = TestRecording::start_with_audio_delays(&delays);
            std::thread::sleep(Duration::from_millis(800));
            if paused {
                recording.control(true);
                std::thread::sleep(Duration::from_millis(500));
            }
            let active = recording.active_seconds();
            recording.finish().unwrap();
            let probe = recording.probe();
            assert_eq!(stream_counts(&probe), (1, delays.len() - 1), "{case}");
            let gap = largest_gap(&probe, "video", f64::INFINITY);
            assert!(gap < 0.1, "{case}: video skips {gap}s");
            let duration = end_seconds(&probe, "h264");
            assert!(
                (duration - active).abs() < 0.2,
                "{case}: video ends at {duration}s after {active}s of active recording"
            );
            let logs = recording.logs();
            assert!(
                logs.iter().any(|log| log.contains(
                    "test audio sent no samples before the recording stopped; the movie has no test audio track"
                )),
                "{case}: {logs:?}"
            );
        }
    }

    #[test]
    fn late_audio_keeps_flowing_while_the_screen_is_idle() {
        let mut recording =
            TestRecording::start_with_audio_delays(&[gst::ClockTime::from_seconds(1)]);
        // An idle screen sends no frames, so when the late track is linked
        // qtmux holds its audio until the next frame arrives.
        let idle = gst::ClockTime::from_mseconds(500)..gst::ClockTime::from_mseconds(2200);
        recording
            .pipeline
            .by_name("video")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                match info.buffer().and_then(|buffer| buffer.pts()) {
                    Some(pts) if idle.contains(&pts) => gst::PadProbeReturn::Drop,
                    _ => gst::PadProbeReturn::Ok,
                }
            });
        let wait_until = |milliseconds| {
            while recording.pipeline.current_running_time().unwrap()
                < gst::ClockTime::from_mseconds(milliseconds)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // Audio that arrives while paused is dropped, so audio stuck behind
        // qtmux before the pause would leave a hole.
        wait_until(2000);
        recording.control(true);
        wait_until(2500);
        recording.control(false);
        wait_until(3500);
        recording.finish().unwrap();
        let probe = recording.probe();
        assert_eq!(stream_counts(&probe), (1, 1));
        let gap = largest_gap(&probe, "audio", f64::INFINITY);
        assert!(gap < 0.1, "audio skips {gap}s");
        let (video, audio) = (end_seconds(&probe, "h264"), end_seconds(&probe, "aac"));
        assert!(
            (video - audio).abs() < 0.15,
            "video ends at {video}s, audio at {audio}s"
        );
    }

    #[test]
    fn a_static_screen_paused_before_audio_arrives_still_finalizes() {
        let mut recording = TestRecording::start_with_audio_delays(&[gst::ClockTime::MAX]);
        // A static screen sends no more frames, so the held video stops
        // growing and only the audio track's end releases it.
        recording
            .pipeline
            .by_name("video")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, |_, info| {
                match info.buffer().map(|buffer| buffer.offset()) {
                    Some(0) => gst::PadProbeReturn::Ok,
                    _ => gst::PadProbeReturn::Drop,
                }
            });
        std::thread::sleep(Duration::from_millis(300));
        let held = recording.pipeline.by_name(HELD_QUEUE).unwrap();
        let level = held.property::<u64>("current-level-time");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(held.property::<u64>("current-level-time"), level);
        recording.control(true);
        std::thread::sleep(Duration::from_millis(500));
        recording.finish().unwrap();
        let probe = recording.probe();
        assert_eq!(stream_counts(&probe), (1, 0));
        let logs = recording.logs();
        assert!(
            logs.iter()
                .any(|log| log.contains("test audio sent no samples before the recording stopped")),
            "{logs:?}"
        );
    }

    #[test]
    fn a_stalled_audio_track_does_not_build_up_encoded_video() {
        let mut recording = TestRecording::start(1);
        let stall = gst::ClockTime::from_mseconds(1000)..gst::ClockTime::from_mseconds(2500);
        recording
            .pipeline
            .iterate_elements()
            .into_iter()
            .filter_map(|element| element.ok())
            .find(|element| {
                element
                    .factory()
                    .is_some_and(|f| f.name() == "audiotestsrc")
            })
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                match info.buffer().and_then(|buffer| buffer.pts()) {
                    Some(pts) if stall.contains(&pts) => gst::PadProbeReturn::Drop,
                    _ => gst::PadProbeReturn::Ok,
                }
            });
        let held = recording.pipeline.by_name(HELD_QUEUE).unwrap();
        let mut most = 0;
        loop {
            let now = recording.pipeline.current_running_time().unwrap();
            if now >= gst::ClockTime::from_mseconds(3000) {
                break;
            }
            // From the stall on; the video held at startup drains before it.
            if now >= gst::ClockTime::from_mseconds(1100) {
                most = most.max(held.property::<u32>("current-level-buffers"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        recording.finish().unwrap();
        recording.probe();
        assert!(most <= 1, "{most} encoded frames waited for stalled audio");
    }

    #[test]
    fn held_video_is_bounded_in_bytes_as_well_as_time() {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::Pipeline::new());
        let video = element("appsrc").unwrap();
        video.set_property_from_str("format", "time");
        video.set_property(
            "caps",
            gst::Caps::builder("video/x-h264")
                .field("stream-format", "avc3")
                .field("alignment", "au")
                .field("width", 320i32)
                .field("height", 180i32)
                .field("framerate", gst::Fraction::new(0, 1))
                .build(),
        );
        let mux = movie_mux().unwrap();
        let sink = element("fakesink").unwrap();
        let silent = element("queue").unwrap();
        pipeline.0.add_many([&video, &mux, &sink, &silent]).unwrap();
        mux.link(&sink).unwrap();
        let mut movie = Movie::new(&pipeline.0, &video, &mux, true).unwrap();
        movie.add_audio("test audio", &silent);
        let held = pipeline.0.by_name(HELD_QUEUE).unwrap();
        pipeline.0.set_state(gst::State::Playing).unwrap();
        // Large constant-QP frames 10 ms apart reach the byte limit long
        // before the time limit.
        let frame = 1 << 20;
        for index in 0..64u64 {
            let pts = gst::ClockTime::from_mseconds(10 * index);
            let mut buffer = gst::Buffer::from_mut_slice(vec![0u8; frame]);
            buffer.get_mut().unwrap().set_pts(pts);
            let before = held.property::<u32>("current-level-bytes");
            assert_eq!(
                video.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
                gst::FlowReturn::Ok
            );
            let deadline = Instant::now() + Duration::from_secs(1);
            while index > 0
                && held.property::<u32>("current-level-bytes") == before
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            let level = held.property::<u32>("current-level-bytes");
            let omitted = movie.link_audio().unwrap();
            if !omitted.is_empty() {
                assert_eq!(omitted, vec![("test audio", false)]);
                assert!(
                    (HELD_VIDEO_BYTES..=HELD_VIDEO_BYTES + (8 << 20)).contains(&level),
                    "left out at {level} held bytes"
                );
                assert!(pts < HELD_VIDEO_LIMIT, "left out at {pts}");
                return;
            }
        }
        panic!(
            "held {} bytes without leaving out the silent track; {:?}",
            held.property::<u32>("current-level-bytes"),
            pipeline
                .0
                .bus()
                .unwrap()
                .pop_filtered(&[gst::MessageType::Error])
        );
    }

    #[test]
    fn audio_that_never_starts_is_left_out_without_dropping_video() {
        let mut recording = TestRecording::start_with_audio_delays(&[gst::ClockTime::MAX]);
        let deadline = Instant::now() + Duration::from_secs(6);
        let omitted = loop {
            match recording
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(RecorderEvent::Log { message, .. }) if message.contains("sent no samples") => {
                    break Some((message, recording.active_seconds()))
                }
                Ok(_) => {}
                Err(_) => break None,
            }
        };
        let (message, at) = omitted.expect("the recording never left out the silent audio track");
        assert!(
            message.contains("while 3 seconds or 32 MiB of video waited for it"),
            "{message}"
        );
        assert!((3.0..3.5).contains(&at), "left out after {at}s");
        std::thread::sleep(Duration::from_millis(500));
        let active = recording.active_seconds();
        recording.finish().unwrap();
        let probe = recording.probe();
        assert_eq!(stream_counts(&probe), (1, 0));
        let gap = largest_gap(&probe, "video", f64::INFINITY);
        assert!(gap < 0.1, "video skips {gap}s");
        let duration = end_seconds(&probe, "h264");
        assert!(
            (duration - active).abs() < 0.2,
            "video ends at {duration}s after {active}s of active recording"
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

    fn test_session() -> RecordingSession {
        static ID: AtomicU64 = AtomicU64::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        RecordingSession {
            id,
            output_path: std::env::temp_dir().join(format!(
                "wrec-linux-record-test-{}-{id}.mov",
                std::process::id()
            )),
        }
    }

    fn silent_settings(codec: Codec) -> RecorderSettings {
        RecorderSettings {
            fps: domain::FrameRate::Fps60,
            codec,
            quality: domain::Quality::High,
            include_system_audio: false,
            include_microphone: false,
            ..RecorderSettings::default()
        }
    }

    #[test]
    fn an_encoder_attempt_reports_its_cleanup_only_after_its_pipeline_is_released() {
        gst::init().unwrap();
        let settings = silent_settings(Codec::H264);
        let attempts = encoding::modes(settings.codec, false).len();
        assert!(attempts > 1, "this test needs two H.264 encoders");
        let sources = Arc::new(Mutex::new(Vec::<gst::glib::WeakRef<gst::Element>>::new()));
        let created = sources.clone();
        let capture = CaptureInput::Element(Box::new(move || {
            let source = element("filesrc").unwrap();
            source.set_property("location", "/nonexistent/wrec-capture-source");
            created.lock().unwrap().push(source.downgrade());
            source
        }));
        let session = test_session();
        let (events, _received) = mpsc::channel();
        let (_commands, receiver) = mpsc::sync_channel(1);
        let (_stop, stopped) = watch::channel(false);
        let signals = Mutex::new(Vec::new());
        let result = record(
            &capture,
            &session,
            &settings,
            &events,
            receiver,
            &stopped,
            &|teardown| {
                let released = sources
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|source| source.upgrade().is_none());
                signals.lock().unwrap().push((teardown, released));
            },
        );
        assert!(result.is_err());
        assert!(!session.output_path.exists());
        let mut expected = Vec::new();
        for _ in 1..attempts {
            expected.push((Teardown::AttemptStarted, false));
            expected.push((Teardown::AttemptFinished, true));
        }
        expected.push((Teardown::Recording, false));
        assert_eq!(signals.into_inner().unwrap(), expected);
    }

    fn variable_rate(rate: i32) -> gst::Caps {
        gst::Caps::builder("video/x-raw")
            .field("format", "BGRx")
            .field("width", 320i32)
            .field("height", 180i32)
            .field("framerate", gst::Fraction::new(0, 1))
            .field("max-framerate", gst::Fraction::new(rate, 1))
            .build()
    }

    struct RateChangeRecording {
        result: Result<()>,
        factory: String,
        session: RecordingSession,
        partial: std::path::PathBuf,
        scheduled: Vec<f64>,
        source_rates: Vec<i32>,
        metrics: RecorderMetrics,
    }

    impl Drop for RateChangeRecording {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.session.output_path);
            let _ = std::fs::remove_file(&self.partial);
        }
    }

    // Each recording paces real frames through a leaky queue; running two at
    // once on a shared encoder would turn scheduling delays into frame drops.
    static RATE_CHANGE_RECORDING: Mutex<()> = Mutex::new(());

    fn record_rate_changes(codec: Codec, phases: &[(i32, u64)]) -> RateChangeRecording {
        gst::init().unwrap();
        let _sequential = RATE_CHANGE_RECORDING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let source = Arc::new(Mutex::new(None::<gst::Element>));
        let source_rates = Arc::new(Mutex::new(Vec::new()));
        let created = source.clone();
        let observed = source_rates.clone();
        let first_rate = phases[0].0;
        let capture = CaptureInput::Element(Box::new(move || {
            let source = element("appsrc").unwrap();
            source.set_property("is-live", true);
            source.set_property_from_str("format", "time");
            source.set_property("caps", variable_rate(first_rate));
            let observed = observed.clone();
            source.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM,
                move |_, info| {
                    if let Some(gst::EventView::Caps(caps)) = info.event().map(|event| event.view())
                    {
                        if let Some(rate) = caps.caps().structure(0).and_then(|structure| {
                            structure.get::<gst::Fraction>("max-framerate").ok()
                        }) {
                            observed.lock().unwrap().push(rate.numer());
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );
            *created.lock().unwrap() = Some(source.clone());
            source
        }));
        let session = test_session();
        let partial = session.output_path.with_extension("partial.mov");
        let (events, received) = mpsc::channel();
        let (_commands, receiver) = mpsc::sync_channel(1);
        let (stop, stopped) = watch::channel(false);
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || {
            record(
                &capture,
                &worker_session,
                &silent_settings(codec),
                &events,
                receiver,
                &stopped,
                &|_| {},
            )
        });
        let appsrc = loop {
            if let Some(source) = source.lock().unwrap().clone() {
                break source;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let playing = Instant::now();
        while appsrc.current_state() != gst::State::Playing
            && playing.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Stamp each frame with its schedule rather than the time the source
        // thread happens to forward it, so every packet maps to one pushed frame.
        let origin = appsrc.current_running_time().unwrap_or_default();
        let start = Instant::now();
        let mut due = Duration::ZERO;
        let mut scheduled = Vec::new();
        'push: for &(rate, seconds) in phases {
            appsrc.set_property("caps", variable_rate(rate));
            for _ in 0..rate as u64 * seconds {
                std::thread::sleep(due.saturating_sub(start.elapsed()));
                let shade = scheduled.len() as u8;
                let mut buffer = gst::Buffer::from_mut_slice(vec![shade; 320 * 180 * 4]);
                let time = origin + gst::ClockTime::from_nseconds(due.as_nanos() as u64);
                let frame = buffer.get_mut().unwrap();
                frame.set_pts(time);
                frame.set_dts(time);
                if appsrc.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer])
                    != gst::FlowReturn::Ok
                {
                    break 'push;
                }
                scheduled.push(due.as_secs_f64());
                due += Duration::from_secs(1) / rate as u32;
            }
        }
        let accepted = Instant::now();
        while appsrc.property::<u64>("current-level-buffers") > 0
            && accepted.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        // qtmux writes a fragment when the next keyframe reaches it, so wait
        // for one that holds frames after the first rate change.
        let changed = phases[0].1 as f64;
        let copied = Instant::now();
        loop {
            std::fs::copy(&session.output_path, &partial).unwrap();
            if packet_span(&partial).is_some_and(|span| span >= changed - 1e-3)
                || copied.elapsed() > Duration::from_secs(10)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        stop.send_replace(true);
        let result = worker.join().unwrap();
        let events: Vec<RecorderEvent> = received.try_iter().collect();
        let factory = events
            .iter()
            .find_map(|event| match event {
                RecorderEvent::Log { message, .. } if message.contains("selected") => message
                    .split("; ")
                    .nth(2)
                    .map(|factory| factory.to_string()),
                _ => None,
            })
            .unwrap();
        let metrics = events
            .iter()
            .rev()
            .find_map(|event| match event {
                RecorderEvent::Metrics { metrics, .. } => Some(metrics.clone()),
                _ => None,
            })
            .unwrap();
        let source_rates = source_rates.lock().unwrap().clone();
        RateChangeRecording {
            result,
            factory,
            session,
            partial,
            scheduled,
            source_rates,
            metrics,
        }
    }

    // Seconds from the first video packet to the last one. Movie timestamps
    // start at the pipeline's running time, not at zero.
    fn packet_span(movie: &Path) -> Option<f64> {
        let output = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
            .args(["packet=pts_time", "-of", "csv=p=0"])
            .arg(movie)
            .output()
            .ok()?;
        let times: Vec<f64> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        let first = times.iter().copied().reduce(f64::min)?;
        times
            .iter()
            .copied()
            .reduce(f64::max)
            .map(|last| last - first)
    }

    fn video_packets(probe: &serde_json::Value) -> Vec<(f64, bool)> {
        let stream = &probe["streams"][0];
        assert_eq!(stream["codec_type"], "video");
        let base: Vec<f64> = stream["time_base"]
            .as_str()
            .unwrap()
            .split('/')
            .map(|part| part.parse().unwrap())
            .collect();
        probe["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|packet| packet["stream_index"] == 0)
            .map(|packet| {
                (
                    packet["pts"].as_i64().unwrap() as f64 * base[0] / base[1],
                    packet["flags"].as_str().unwrap().starts_with('K'),
                )
            })
            .collect()
    }

    fn decoded_video_frames(movie: &Path) -> u64 {
        let output = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames"])
            .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
            .arg(movie)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap()
    }

    fn assert_rate_changes_stay_playable(codec: Codec, name: &str, phases: &[(i32, u64)]) {
        let recording = record_rate_changes(codec, phases);
        let movie = &recording.session.output_path;
        assert!(recording.result.is_ok(), "{:?}", recording.result);
        let rates: Vec<i32> = phases.iter().map(|(rate, _)| *rate).collect();
        let mut transitions = recording.source_rates.clone();
        transitions.dedup();
        assert_eq!(transitions, rates, "the source did not change its caps");
        let probe = probe_movie(movie);
        assert_eq!(probe["streams"][0]["codec_name"], name);
        let packets = video_packets(&probe);
        let encoded = recording.metrics.frames.unwrap();
        let dropped = recording.metrics.dropped_frames.unwrap();
        assert_eq!(packets.len() as u64, encoded);
        assert_eq!(encoded + dropped, recording.scheduled.len() as u64);
        assert_eq!(decoded_video_frames(movie), packets.len() as u64);
        let first = packets[0].0;
        let va = recording.factory.starts_with("va");
        let mut phase_start = 0.0;
        let mut remaining = packets
            .iter()
            .map(|(pts, key)| (pts - first, *key))
            .peekable();
        for &(rate, seconds) in phases {
            let phase_end = phase_start + seconds as f64;
            let mut phase = Vec::new();
            while let Some((position, key)) =
                remaining.next_if(|(position, _)| *position < phase_end - 0.5 / rate as f64)
            {
                assert!(
                    recording
                        .scheduled
                        .iter()
                        .any(|due| (position - due).abs() < 2e-6),
                    "a packet at {position}s matches no pushed frame"
                );
                phase.push(key);
            }
            let interval = if va {
                keyframe_interval(&variable_rate(rate), 60) as usize
            } else {
                120
            };
            let keyframes: Vec<usize> = phase
                .iter()
                .enumerate()
                .filter_map(|(index, key)| key.then_some(index))
                .collect();
            assert_eq!(
                keyframes,
                (0..phase.len()).step_by(interval).collect::<Vec<_>>(),
                "{} at {rate} fps from {phase_start}s",
                recording.factory
            );
            phase_start = phase_end;
        }
        assert!(remaining.next().is_none(), "packets after the last phase");
        let last_frame = 1.0 / phases.last().unwrap().0 as f64;
        let span = packets.last().unwrap().0 - first + last_frame;
        let duration: f64 = probe["streams"][0]["duration"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (duration - span).abs() < 1e-3,
            "the movie lasts {duration}s, but its last frame ends at {span}s"
        );
        for probe in [&probe, &probe_movie(&recording.partial)] {
            assert_eq!(
                probe["streams"][0]["codec_tag_string"],
                if name == "h264" { "avc3" } else { "hev1" }
            );
            assert!(
                !probe.to_string().contains("New Extradata"),
                "the movie switches sample descriptions"
            );
        }
        let partial = probe_movie(&recording.partial);
        let partial_packets = video_packets(&partial);
        let span = partial_packets.last().unwrap().0 - partial_packets[0].0;
        assert!(
            span >= phases[0].1 as f64 - 1e-3,
            "the partial movie ends {span}s in, before the first rate change"
        );
        assert_eq!(
            decoded_video_frames(&recording.partial),
            partial_packets.len() as u64
        );
        eprintln!(
            "{} {rates:?}: {} frames, {dropped} dropped; partial movie has {} frames",
            recording.factory,
            packets.len(),
            partial_packets.len()
        );
    }

    const RISING: [(i32, u64); 3] = [(5, 3), (60, 6), (5, 3)];
    const FALLING: [(i32, u64); 3] = [(60, 3), (5, 4), (60, 3)];

    #[test]
    fn h264_recordings_survive_capture_rate_changes() {
        assert_rate_changes_stay_playable(Codec::H264, "h264", &RISING);
        assert_rate_changes_stay_playable(Codec::H264, "h264", &FALLING);
    }

    #[test]
    fn hevc_recordings_survive_capture_rate_changes() {
        assert_rate_changes_stay_playable(Codec::Hevc, "hevc", &RISING);
        assert_rate_changes_stay_playable(Codec::Hevc, "hevc", &FALLING);
    }

    // Rust heap bytes allocated and not yet freed by each thread. GStreamer
    // buffers come from GLib and are not counted.
    struct CountingAllocator;

    thread_local! {
        static LIVE_BYTES: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let _ = LIVE_BYTES.try_with(|live| live.set(live.get() + layout.size() as isize));
            unsafe { std::alloc::System.alloc(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
            let _ = LIVE_BYTES.try_with(|live| live.set(live.get() - layout.size() as isize));
            unsafe { std::alloc::System.dealloc(pointer, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    struct QueueDrops {
        dropped: u64,
        dropped_while_stuck: u64,
        delivered: u64,
        heap_growth: isize,
    }

    // Pushes frames straight into the capture queue while every frame leaving
    // it waits for a permit, then reports the frames dropped and delivered and
    // the heap the pushing thread kept while the consumer was stuck.
    fn queue_drops(permit_during_overrun: bool, frames: u64) -> QueueDrops {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::Pipeline::new());
        let queue = video_queue().unwrap();
        let sink = element("fakesink").unwrap();
        sink.set_property("sync", false);
        pipeline.0.add_many([&queue, &sink]).unwrap();
        queue.link(&sink).unwrap();
        let counters = Arc::new(Counters::default());
        count_dropped(&queue, counters.clone());
        let (permits, permitted) = mpsc::channel::<()>();
        let permitted = Mutex::new(permitted);
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let counted = delivered.clone();
        queue
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let _ = permitted.lock().unwrap().recv();
                if let Some(buffer) = info.buffer() {
                    counted.lock().unwrap().push((
                        buffer.as_ptr() as usize,
                        buffer.flags().contains(gst::BufferFlags::DISCONT),
                    ));
                }
                gst::PadProbeReturn::Ok
            });
        if permit_during_overrun {
            let permits = Mutex::new(permits.clone());
            queue.connect("overrun", false, move |values| {
                let queue = values[0].get::<gst::Element>().unwrap();
                permits.lock().unwrap().send(()).unwrap();
                while queue.property::<u32>("current-level-buffers") >= 2 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                None
            });
        }
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let input = queue.static_pad("sink").unwrap();
        input.send_event(gst::event::StreamStart::new("queue-drops"));
        input.send_event(gst::event::Caps::new(&variable_rate(60)));
        input.send_event(gst::event::Segment::new(&gst::FormattedSegment::<
            gst::ClockTime,
        >::new()));
        // Keep a reference to every frame, as pipewiresrc keeps its last one,
        // so a counter that wrote to a frame would have to copy it. The queue
        // itself copies only the frame after a discard, to mark it DISCONT.
        let mut sent = Vec::with_capacity(frames as usize);
        let heap = LIVE_BYTES.with(|live| live.get());
        for frame in 0..frames {
            let mut buffer = gst::Buffer::new();
            buffer
                .get_mut()
                .unwrap()
                .set_pts(gst::ClockTime::from_mseconds(frame * 10));
            sent.push(buffer.clone());
            assert_eq!(input.chain(buffer), Ok(gst::FlowSuccess::Ok));
            if permit_during_overrun || frame == 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let heap_growth = LIVE_BYTES.with(|live| live.get()) - heap;
        let dropped_while_stuck = dropped_frames(&pipeline.0, &counters);
        for _ in 0..frames {
            let _ = permits.send(());
        }
        input.send_event(gst::event::Eos::new());
        pipeline
            .0
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Eos])
            .unwrap();
        let sent: std::collections::HashSet<usize> =
            sent.iter().map(|buffer| buffer.as_ptr() as usize).collect();
        let delivered = delivered.lock().unwrap();
        assert!(
            delivered
                .iter()
                .all(|(buffer, discont)| *discont || sent.contains(buffer)),
            "the queue delivered a copy of a captured frame"
        );
        QueueDrops {
            dropped: dropped_frames(&pipeline.0, &counters),
            dropped_while_stuck,
            delivered: delivered.len() as u64,
            heap_growth,
        }
    }

    #[test]
    fn dropped_frames_count_only_frames_the_queue_discards() {
        let raced = queue_drops(true, 6);
        assert_eq!((raced.dropped, raced.delivered), (0, 6));
        let leaked = queue_drops(false, 6);
        assert_eq!((leaked.dropped, leaked.delivered), (3, 3));
    }

    #[test]
    fn a_stuck_encoder_does_not_grow_drop_counting() {
        let stuck = queue_drops(false, 2000);
        assert_eq!(stuck.dropped_while_stuck, 1997);
        assert_eq!((stuck.dropped, stuck.delivered), (1997, 3));
        assert!(
            stuck.heap_growth < 4096,
            "counting kept {} bytes for 1997 dropped frames",
            stuck.heap_growth
        );
    }

    // Durations the movie receives for encoded frames that arrive with
    // `duration` under each of the given encoder caps.
    fn movie_durations(
        caps: &[&str],
        duration: Option<gst::ClockTime>,
    ) -> Vec<Option<gst::ClockTime>> {
        gst::init().unwrap();
        let pipeline = PipelineGuard(gst::Pipeline::new());
        let format = movie_format(Codec::H264, 60).unwrap();
        let sink = element("fakesink").unwrap();
        pipeline.0.add_many([&format, &sink]).unwrap();
        format.link(&sink).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let recorded = received.clone();
        sink.static_pad("sink")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                recorded
                    .lock()
                    .unwrap()
                    .push(info.buffer().and_then(|buffer| buffer.duration()));
                gst::PadProbeReturn::Ok
            });
        pipeline.0.set_state(gst::State::Playing).unwrap();
        let input = format.static_pad("sink").unwrap();
        input.send_event(gst::event::StreamStart::new("movie-durations"));
        for (index, caps) in caps.iter().enumerate() {
            input.send_event(gst::event::Caps::new(&caps.parse::<gst::Caps>().unwrap()));
            if index == 0 {
                input.send_event(gst::event::Segment::new(&gst::FormattedSegment::<
                    gst::ClockTime,
                >::new()));
            }
            let mut buffer = gst::Buffer::new();
            buffer.get_mut().unwrap().set_duration(duration);
            assert_eq!(input.chain(buffer), Ok(gst::FlowSuccess::Ok));
        }
        let received = received.lock().unwrap().clone();
        received
    }

    #[test]
    fn frames_without_a_duration_last_one_frame_at_the_capture_rate() {
        let encoded = "video/x-h264, stream-format=avc3, alignment=au, width=320, height=180";
        let rated = |rate: &str| format!("{encoded}, framerate=0/1, max-framerate={rate}");
        let frame = |rate: u64| Some(gst::ClockTime::SECOND / rate);
        assert_eq!(
            movie_durations(&[&rated("5/1"), &rated("60/1"), encoded], None),
            [frame(5), frame(60), frame(60)]
        );
        assert_eq!(
            movie_durations(&[&format!("{encoded}, framerate=30/1"), encoded], None),
            [frame(30), frame(60)]
        );
        let kept = gst::ClockTime::from_mseconds(7);
        assert_eq!(
            movie_durations(&[&rated("5/1"), encoded], Some(kept)),
            [Some(kept), Some(kept)]
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
        // The last case loses the source while video waits for audio.
        for (paused, audio) in [
            (false, gst::ClockTime::ZERO),
            (true, gst::ClockTime::ZERO),
            (false, gst::ClockTime::MAX),
        ] {
            let mut recording = TestRecording::start_with_audio_delays(&[audio]);
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
