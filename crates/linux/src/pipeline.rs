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
        "mp4mux",
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

// qtmux guesses a 4:3 or 16:9 TV picture for widths 641 through 1052 and
// heights 480 through 576, and writes a clean aperture that makes players crop
// a window of, say, 800x528 to 704x528. mp4mux is the same muxer writing the
// ISO flavor, which declares the full picture.
fn movie_mux() -> Result<gst::Element> {
    let mux = element("mp4mux")?;
    mux.set_property("fragment-duration", 10000u32);
    mux.set_property_from_str("fragment-mode", "first-moov-then-finalise");
    // Preserve sub-frame timestamps around pause/resume instead of rounding
    // them to the default frame-rate-derived track timescale.
    mux.set_property("trak-timescale", 1_000_000u32);
    Ok(mux)
}

const WRITE_QUEUE: &str = "write-queue";

// mp4mux writes each sample as it muxes it, so while a write blocks every
// track waits behind it: video drops at capture within two frames, and audio
// once its queue fills. This buffer stays empty unless the disk stalls; at
// the highest bitrate-mode quality (16 Mbit/s) it covers about 16 seconds.
const WRITE_BUFFER_BYTES: u32 = 32 << 20;

fn movie_file(pipeline: &gst::Pipeline, mux: &gst::Element, location: &str) -> Result<()> {
    let queue = element("queue")?;
    queue.set_property("name", WRITE_QUEUE);
    queue.set_property("max-size-bytes", WRITE_BUFFER_BYTES);
    queue.set_property("max-size-buffers", 0u32);
    queue.set_property("max-size-time", 0u64);
    let sink = element("filesink")?;
    sink.set_property("location", location);
    sink.set_property("sync", false);
    pipeline.add_many([&queue, &sink]).map_err(backend)?;
    gst::Element::link_many([mux, &queue, &sink]).map_err(backend)
}

const CAPTURE_QUEUE: &str = "capture-queue";

// pipewiresrc resends the last frame after this long without a new one, so an
// idle screen still sends a frame each keepalive.
const KEEPALIVE: gst::ClockTime = gst::ClockTime::SECOND;

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
    // Finished pauses, in running time, oldest first.
    pauses: Vec<(gst::ClockTime, gst::ClockTime)>,
}

impl Timeline {
    fn pause(&mut self, now: gst::ClockTime) {
        self.paused_at.get_or_insert(now);
    }

    fn resume(&mut self, now: gst::ClockTime) {
        if let Some(paused_at) = self.paused_at.take() {
            self.offset += now.saturating_sub(paused_at);
            self.accept_from = now;
            self.pauses.push((paused_at, now));
        }
    }

    // How much of the running time from `start` to `end` was paused.
    fn paused_between(&self, start: gst::ClockTime, end: gst::ClockTime) -> gst::ClockTime {
        self.paused_at
            .map(|at| (at, gst::ClockTime::MAX))
            .into_iter()
            .chain(self.pauses.iter().rev().copied())
            .take_while(|&(_, resumed)| resumed > start)
            .map(|(paused, resumed)| end.min(resumed).saturating_sub(start.max(paused)))
            .sum()
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
    // The movie is complete on disk; native cleanup follows.
    Finalized,
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
            source.set_property("keepalive-time", KEEPALIVE.mseconds() as i32);
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
    let mut chain = vec![&source, &input, &queue];
    chain.extend(converters.iter());
    chain.extend([&output, &encoder, &parser, &format]);
    pipeline
        .0
        .add_many(chain.iter().copied())
        .map_err(backend)?;
    pipeline.0.add(&mux).map_err(backend)?;
    gst::Element::link_many(chain.iter().copied()).map_err(backend)?;
    movie_file(
        &pipeline.0,
        &mux,
        session
            .output_path
            .to_str()
            .ok_or_else(|| backend("recording output path must be UTF-8"))?,
    )?;
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
            "capture-engine: selected {}; capture queue limited to 2 frames; {} MiB write buffer",
            mode.description(),
            WRITE_BUFFER_BYTES >> 20
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
    (attempt.stopping)(if result.is_ok() {
        Teardown::Finalized
    } else if ending {
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
    source: gst::Element,
    pad: gst::Pad,
    probe: Option<gst::PadProbeId>,
    state: Arc<AtomicU8>,
    // Nanoseconds of audio missing between captured buffers.
    lost: Arc<AtomicU64>,
    // Where in the movie the track failed, and why.
    failed: Option<(gst::ClockTime, String)>,
}

// The recording pipeline and the tracks of its movie. mp4mux writes a track
// for every pad requested from it, and a track that never received audio has
// no sample description, which makes the whole movie unreadable. AAC encoders
// only learn their format from their first input buffer, so audio pads are
// requested once each track has delivered a buffer or ended without one.
// Until then video waits in a queue instead of being dropped at capture.
// mp4mux takes nothing while any track is empty, so afterwards the same queue
// lets video wait for a stalled audio track as long as audio can wait for
// video, instead of dropping frames at capture.
struct Movie {
    pipeline: gst::Pipeline,
    mux: gst::Element,
    video: gst::Pad,
    held: Option<(gst::Element, gst::PadProbeId)>,
    audio: Vec<AudioTrack>,
}

impl Movie {
    fn new(
        pipeline: &gst::Pipeline,
        video: &gst::Element,
        mux: &gst::Element,
        audio: bool,
    ) -> Result<Self> {
        let track = mux
            .request_pad_simple("video_%u")
            .ok_or_else(|| backend("the movie muxer refused a video track"))?;
        let mut movie = Self {
            pipeline: pipeline.clone(),
            mux: mux.clone(),
            video: track.clone(),
            held: None,
            audio: Vec::new(),
        };
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

    fn add_audio(
        &mut self,
        name: &'static str,
        queue: &gst::Element,
        source: &gst::Element,
        lost: Arc<AtomicU64>,
    ) {
        let pad = queue.static_pad("src").unwrap();
        // Linking the track sends a reconfigure upstream. The encoder answers
        // with an allocation query, which waits behind this queue while mp4mux
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
            source: source.clone(),
            pad,
            probe,
            state,
            lost,
            failed: None,
        });
    }

    // Called on every pass of the run loop. Requests the audio pads once every
    // track has started or ended, or the held video reached its limit, then
    // releases the video. Returns the omitted tracks, each with whether it
    // ended before delivering audio.
    fn link_audio(&mut self) -> Result<Vec<(&'static str, bool)>> {
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
        Ok(omitted)
    }

    // The audio track whose source posted a message, if any.
    fn audio_source(&mut self, message: &gst::Message) -> Option<&mut AudioTrack> {
        let src = message.src()?;
        self.audio.iter_mut().find(|track| {
            src == track.source.upcast_ref::<gst::Object>() || src.has_as_ancestor(&track.source)
        })
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
    // Counted before pausing drops anything.
    let lost = Arc::new(AtomicU64::new(0));
    count_lost_audio(source, lost.clone(), counters.clone());
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
    // track to start, which can take HELD_VIDEO_LIMIT. mp4mux writes a video
    // frame only when the next one arrives and holds later audio until then,
    // so at an idle screen's keepalive rate audio waits up to two keepalives,
    // and a pause that swallows a frame makes it almost three.
    queue.set_property(
        "max-size-time",
        (HELD_VIDEO_LIMIT.max(KEEPALIVE * 3) + gst::ClockTime::SECOND).nseconds(),
    );
    queue.set_property("max-size-buffers", 0u32);
    queue.set_property("max-size-bytes", 0u32);
    let chain = [
        source, &convert, &resample, &caps, &encoder, &parser, &queue,
    ];
    movie.pipeline.add_many(chain).map_err(backend)?;
    gst::Element::link_many(chain).map_err(backend)?;
    movie.add_audio(name, &queue, source, lost);
    Ok(())
}

// pulsesrc keeps buffers contiguous unless it skipped audio nobody read in
// time, so a longer jump is lost audio.
const AUDIO_HOLE: gst::ClockTime = gst::ClockTime::from_mseconds(20);

// Counts audio missing between consecutive captured buffers, before pausing
// drops any. When downstream does not read pulsesrc in time, its ring buffer
// overwrites the oldest audio, and the next buffer is stamped where capture
// actually resumed. Sources keep capturing while paused, so a pause leaves no
// gap, and the paused part of a gap is not in the movie. Capture timestamps
// and pauses are both in running time.
fn count_lost_audio(source: &gst::Element, lost: Arc<AtomicU64>, counters: Arc<Counters>) {
    // Where the next buffer should start.
    let next = Mutex::new(None::<gst::ClockTime>);
    source
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let Some((pts, duration)) = info
                .buffer()
                .and_then(|buffer| buffer.pts().zip(buffer.duration()))
            else {
                return gst::PadProbeReturn::Ok;
            };
            let mut next = next.lock().unwrap();
            if let Some(expected) = next.filter(|&expected| pts > expected) {
                let paused = counters
                    .timeline
                    .lock()
                    .unwrap()
                    .paused_between(expected, pts);
                let missing = pts.saturating_sub(expected).saturating_sub(paused);
                if missing > AUDIO_HOLE {
                    lost.fetch_add(missing.nseconds(), Ordering::Relaxed);
                }
            }
            *next = Some(pts + duration);
            gst::PadProbeReturn::Ok
        });
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
// sets. mp4mux stores those as an extra sample description, which a fragmented
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
    // mp4mux times each sample by the next one and takes the last sample's
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
                                // A movie track has a fixed canvas. Window resizes are
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
    let mut omitted = Vec::new();
    let mut logged_lost = vec![0; movie.audio.len()];
    let mut most_buffered = 0;
    let mut video_cut = false;
    let log = |message: String| {
        let _ = events.send(RecorderEvent::Log {
            session_id: Some(session.id),
            message,
        });
    };
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
            // A source pushing into a full queue holds its stream lock, and
            // sending it EOS waits for that lock, so wait here instead.
            let ending = pipeline.clone();
            std::thread::spawn(move || ending.send_event(gst::event::Eos::new()));
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
                    // A failed audio source, such as pulsesrc after its server
                    // went away, ends its own track, and the movie keeps
                    // recording the others. Other audio errors fail the job.
                    if let Some(track) = movie.audio_source(&message) {
                        if track.failed.is_none() {
                            // pulsesrc ends its branch after a fatal error; make
                            // sure, without waiting on its stream lock here.
                            if let Some(branch) =
                                track.source.static_pad("src").and_then(|pad| pad.peer())
                            {
                                std::thread::spawn(move || {
                                    branch.send_event(gst::event::Eos::new())
                                });
                            }
                            let position = counters
                                .timeline
                                .lock()
                                .unwrap()
                                .position(pipeline.current_running_time().unwrap_or_default());
                            log(format!(
                                "capture-engine: {} stopped at {:.1} s: {}; its track ends there and the recording continues",
                                track.name,
                                seconds(position),
                                error.error()
                            ));
                            track.failed = Some((position, error.error().to_string()));
                        }
                        continue;
                    }
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
                    if let Some(message) = media_lost(
                        movie,
                        &omitted,
                        dropped_frames(pipeline, counters),
                        video_cut,
                    ) {
                        let _ = events.send(RecorderEvent::MediaLost {
                            session_id: session.id,
                            message,
                        });
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        for (name, ended) in movie.link_audio()? {
            log(format!(
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
            ));
            omitted.push(name);
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
        if stopping.is_some_and(|at| at.elapsed() > FINALIZING) {
            // Nothing is waiting for the disk or for audio, yet video has
            // not reached the muxer: the encoder or capture is stuck. End
            // the video track there, so the muxer can finish the movie with
            // what it has.
            if !video_cut
                && !movie.video.pad_flags().contains(gst::PadFlags::EOS)
                && downstream_idle(pipeline)
            {
                video_cut = true;
                log(format!(
                    "capture-engine: video did not reach the movie writer within {} s of stop while nothing waited downstream; ending the video track there so the movie can be finalized",
                    FINALIZING.as_secs()
                ));
                let video = movie.video.clone();
                std::thread::spawn(move || video.send_event(gst::event::Eos::new()));
            }
            if !video_cut || stopping.is_some_and(|at| at.elapsed() > FINALIZING * 3 / 2) {
                return Err(backend(format!(
                    "Movie finalization timed out after {}s; only completed fragments may be playable.",
                    stopping.unwrap().elapsed().as_secs()
                )));
            }
        }
        if started && last_metrics.elapsed() >= Duration::from_secs(1) {
            emit_metrics(pipeline, session, events, counters);
            let position = counters
                .timeline
                .lock()
                .unwrap()
                .position(pipeline.current_running_time().unwrap_or_default());
            for (track, logged) in movie.audio.iter().zip(&mut logged_lost) {
                let lost = track.lost.load(Ordering::Relaxed);
                if lost > *logged {
                    log(format!(
                        "capture-engine: {} lost {:.2} s of audio before {:.1} s; the movie has a gap there",
                        track.name,
                        (lost - *logged) as f64 / 1e9,
                        seconds(position)
                    ));
                    *logged = lost;
                }
            }
            // The allocator keeps memory freed after a stall drains its
            // buffers until it is asked to return it.
            let buffered = buffered_bytes(pipeline);
            if buffered < STALL_DRAINED && most_buffered >= STALL_BUFFERED {
                release_freed_memory();
                most_buffered = 0;
            }
            most_buffered = most_buffered.max(buffered);
            last_metrics = Instant::now();
        }
    }
}

// How long finalization may take before a stuck video track is ended at the
// muxer, which then has half as long again.
const FINALIZING: Duration = Duration::from_secs(10);

// Nothing waits for the disk, and no video waits for audio.
fn downstream_idle(pipeline: &gst::Pipeline) -> bool {
    buffered_bytes(pipeline) < STALL_DRAINED
        && pipeline.by_name(HELD_QUEUE).map_or(true, |queue| {
            queue.property::<u32>("current-level-buffers") == 0
        })
}

const STALL_BUFFERED: u64 = 8 << 20;
const STALL_DRAINED: u64 = 1 << 20;

// Encoded media waiting for the disk or for a stalled track.
fn buffered_bytes(pipeline: &gst::Pipeline) -> u64 {
    [WRITE_QUEUE, HELD_QUEUE]
        .into_iter()
        .filter_map(|name| pipeline.by_name(name))
        .map(|queue| u64::from(queue.property::<u32>("current-level-bytes")))
        .sum()
}

fn seconds(time: gst::ClockTime) -> f64 {
    time.nseconds() as f64 / 1e9
}

// What the finished movie is missing, if anything. A movie that plays is not
// necessarily whole.
fn media_lost(
    movie: &Movie,
    omitted: &[&str],
    dropped_frames: u64,
    video_cut: bool,
) -> Option<String> {
    let mut lost = Vec::new();
    if video_cut {
        lost.push(format!(
            "video did not finish within {} s of stop, so the video ends early",
            FINALIZING.as_secs()
        ));
    }
    if dropped_frames > 0 {
        lost.push(format!("{dropped_frames} video frames were dropped"));
    }
    for track in &movie.audio {
        let missing = track.lost.load(Ordering::Relaxed);
        if missing > 0 {
            lost.push(format!(
                "{} is missing {:.2} s of audio",
                track.name,
                missing as f64 / 1e9
            ));
        }
        if let Some((position, reason)) = &track.failed {
            lost.push(format!(
                "{} stopped at {:.1} s ({reason}), so its track ends there",
                track.name,
                seconds(*position)
            ));
        } else if omitted.contains(&track.name) {
            lost.push(format!(
                "{} sent no samples, so the movie has no {} track",
                track.name, track.name
            ));
        }
    }
    (!lost.is_empty()).then(|| format!("The movie finished but lost media: {}.", lost.join("; ")))
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
        // When run() returned, before the pipeline was set to NULL.
        returned: Arc<Mutex<Option<Instant>>>,
    }

    impl TestRecording {
        fn start(audio_tracks: usize) -> Self {
            Self::start_with_audio_delays(&vec![gst::ClockTime::ZERO; audio_tracks])
        }

        fn start_with_audio_delays(audio_delays: &[gst::ClockTime]) -> Self {
            Self::start_with(audio_delays, false)
        }

        fn start_with(audio_delays: &[gst::ClockTime], counting_encoder: bool) -> Self {
            Self::start_from(
                MOVING_VIDEO,
                audio_delays
                    .iter()
                    .map(|&delay| ("test audio", delayed_audio(delay)))
                    .collect(),
                counting_encoder,
            )
        }

        fn start_with_audio(audio: Vec<(&'static str, gst::Element)>) -> Self {
            Self::start_from(MOVING_VIDEO, audio, false)
        }

        // An idle screen: one frame per keepalive, stamped on arrival, and
        // no source latency, like pipewiresrc.
        fn start_idle(audio_tracks: usize) -> Self {
            Self::start_from(
                "appsrc name=video is-live=true format=time do-timestamp=true caps=video/x-raw,format=I420,width=320,height=180,framerate=0/1 ! capsfilter name=capture caps=video/x-raw,format=I420,width=320,height=180,framerate=0/1",
                (0..audio_tracks)
                    .map(|_| ("test audio", delayed_audio(gst::ClockTime::ZERO)))
                    .collect(),
                false,
            )
        }

        fn start_from(
            video: &str,
            audio: Vec<(&'static str, gst::Element)>,
            counting_encoder: bool,
        ) -> Self {
            gst::init().unwrap();
            static ID: AtomicU64 = AtomicU64::new(0);
            let id = ID.fetch_add(1, Ordering::Relaxed);
            let session = RecordingSession {
                id,
                output_path: std::env::temp_dir()
                    .join(format!("wrec-linux-test-{}-{id}.mp4", std::process::id())),
            };
            // Synthetic software encoding is confined to tests. Exercise the same
            // bus/control/mux code without pretending this is a hardware benchmark.
            let pipeline = gst::parse::launch(&format!(
                "{video} openh264enc name=encoder ! h264parse name=parser"
            ))
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
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
            pipeline.add(&mux).unwrap();
            movie_file(&pipeline, &mux, session.output_path.to_str().unwrap()).unwrap();
            let mut movie = Movie::new(
                &pipeline,
                &pipeline.by_name("parser").unwrap(),
                &mux,
                !audio.is_empty(),
            )
            .unwrap();
            let counters = Arc::new(Counters::default());
            count_dropped(&queue, counters.clone());
            let video = pipeline.by_name("video").unwrap();
            attach_timing_probe(&video, counters.clone());
            if video
                .factory()
                .is_some_and(|factory| factory.name() == "appsrc")
            {
                std::thread::spawn(move || loop {
                    let frame = gst::Buffer::from_mut_slice(vec![0u8; 320 * 180 * 3 / 2]);
                    if video.emit_by_name::<gst::FlowReturn>("push-buffer", &[&frame])
                        != gst::FlowReturn::Ok
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_nanos(KEEPALIVE.nseconds()));
                });
            }
            let encoder = pipeline.by_name("encoder").unwrap();
            if counting_encoder {
                count_frames_like_old_va_encoders(&pipeline.by_name("video").unwrap(), &encoder);
            }
            keep_capture_timestamps(&encoder, &pipeline.by_name("parser").unwrap());
            for (name, source) in &audio {
                add_audio_source(&mut movie, source, name, &counters).unwrap();
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
            let returned = Arc::new(Mutex::new(None));
            let worker_returned = returned.clone();
            let worker = std::thread::spawn(move || {
                let _guard = PipelineGuard(worker_pipeline);
                let result = run(
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
                );
                *worker_returned.lock().unwrap() = Some(Instant::now());
                result
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
                returned,
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
            // A recording that already failed has dropped its receiver.
            let _ = self.stop.send(true);
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

    const MOVING_VIDEO: &str = "videotestsrc name=video is-live=true pattern=ball ! capsfilter name=capture caps=video/x-raw,width=320,height=180,framerate=30/1";

    // Audio that starts `delay` into the recording.
    fn delayed_audio(delay: gst::ClockTime) -> gst::Element {
        gst::init().unwrap();
        let source = element("audiotestsrc").unwrap();
        source.set_property("is-live", true);
        source
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                match info.buffer().and_then(|buffer| buffer.pts()) {
                    Some(pts) if pts < delay => gst::PadProbeReturn::Drop,
                    _ => gst::PadProbeReturn::Ok,
                }
            });
        source
    }

    // Live capture like pulsesrc: the producer keeps real time, and when
    // nobody reads its 200 ms ring buffer in time it overwrites the oldest
    // audio and marks the next buffer DISCONT. A leaky queue does the same.
    fn ring_buffer_audio(name: &str) -> gst::Element {
        audio_bin(
            name,
            "queue leaky=downstream max-size-time=200000000 max-size-buffers=0 max-size-bytes=0",
        )
    }

    // Audio that reaches the pipeline `lag` after it was captured. Unlike
    // the ring buffer it never drops audio, however long its reader takes.
    fn lagging_audio(name: &str, lag: Duration) -> gst::Element {
        audio_bin(
            name,
            &format!(
                "queue max-size-time=0 max-size-buffers=0 max-size-bytes=0 min-threshold-time={}",
                lag.as_nanos()
            ),
        )
    }

    fn audio_bin(name: &str, queue: &str) -> gst::Element {
        gst::init().unwrap();
        let bin = gst::parse::bin_from_description(
            &format!("audiotestsrc name=producer is-live=true samplesperbuffer=480 ! {queue}"),
            true,
        )
        .unwrap();
        bin.set_property("name", name);
        bin.upcast()
    }

    // Blocks the pad's streaming thread once, `after` from now, for `stall`,
    // like a write() or an encoder call that does not return.
    fn stall_once(pad: &gst::Pad, after: Duration, stall: Duration) {
        let start = Instant::now();
        let done = std::sync::atomic::AtomicBool::new(false);
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if start.elapsed() >= after && !done.swap(true, Ordering::SeqCst) {
                std::thread::sleep(stall);
            }
            gst::PadProbeReturn::Ok
        });
    }

    // Seconds missing from each audio track. mp4mux stretches the packet
    // before a hole over it, so a hole is packet spacing beyond one AAC frame.
    fn audio_holes(probe: &serde_json::Value) -> Vec<f64> {
        let packets = probe["packets"].as_array().unwrap();
        probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|stream| stream["codec_type"] == "audio")
            .map(|stream| {
                let mut starts: Vec<f64> = packets
                    .iter()
                    .filter(|packet| packet["stream_index"] == stream["index"])
                    .map(|packet| packet["pts_time"].as_str().unwrap().parse().unwrap())
                    .collect();
                starts.sort_by(f64::total_cmp);
                starts
                    .windows(2)
                    .map(|pair| pair[1] - pair[0] - 1024.0 / 48000.0)
                    .filter(|hole| *hole > 0.03)
                    .sum::<f64>()
                    .max(0.0)
            })
            .collect()
    }

    // Seconds of 30 fps video missing between frames.
    fn missing_video(probe: &serde_json::Value) -> f64 {
        let index = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .unwrap()["index"]
            .clone();
        let mut pts: Vec<f64> = probe["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|packet| packet["stream_index"] == index)
            .map(|packet| packet["pts_time"].as_str().unwrap().parse().unwrap())
            .collect();
        pts.sort_by(f64::total_cmp);
        pts.windows(2)
            .map(|pair| pair[1] - pair[0] - 1.0 / 30.0)
            .filter(|missing| *missing > 0.02)
            .sum()
    }

    fn pipeline_element(pipeline: &gst::Pipeline, factory: &str) -> Vec<gst::Element> {
        pipeline
            .iterate_recurse()
            .into_iter()
            .flatten()
            .filter(|element| {
                element
                    .factory()
                    .is_some_and(|found| found.name() == factory)
            })
            .collect()
    }

    #[derive(Clone, Copy, Debug)]
    enum Stall {
        // write() into the movie file blocks.
        Write,
        // The video encoder takes no frame.
        Encoder,
        // The microphone captures nothing.
        AudioProducer,
        // The microphone's server goes away, like pulsesrc after a
        // PipeWire or PulseAudio restart.
        AudioFailure,
    }

    struct StallOutcome {
        result: Result<()>,
        // Seconds missing from system audio and the microphone.
        holes: Vec<f64>,
        // Where each audio track ends.
        audio_ends: Vec<f64>,
        video_end: f64,
        missing_video: f64,
        dropped: u64,
        logs: Vec<String>,
        media_lost: Option<String>,
    }

    impl StallOutcome {
        // The loss the recording reported for a track, in seconds.
        fn reported(&self, track: &str) -> f64 {
            let message = self.media_lost.as_deref().unwrap_or_default();
            message
                .split_once(&format!("{track} is missing "))
                .and_then(|(_, rest)| rest.split_once(" s")?.0.parse().ok())
                .unwrap_or(0.0)
        }
    }

    // System audio and a microphone, a stall two seconds in, then four more
    // seconds. `adjust` changes the pipeline before the stall.
    fn record_through(
        stall: Stall,
        seconds: f64,
        adjust: impl FnOnce(&gst::Pipeline),
    ) -> StallOutcome {
        let mut recording = TestRecording::start_with_audio(vec![
            ("system audio", ring_buffer_audio("system-audio")),
            ("microphone", ring_buffer_audio("microphone")),
        ]);
        let pipeline = &recording.pipeline;
        adjust(pipeline);
        let (after, length) = (Duration::from_secs(2), Duration::from_secs_f64(seconds));
        let microphone = pipeline
            .by_name("microphone")
            .unwrap()
            .downcast::<gst::Bin>()
            .unwrap()
            .by_name("producer")
            .unwrap();
        match stall {
            Stall::Write => {
                let sink = &pipeline_element(pipeline, "filesink")[0];
                stall_once(&sink.static_pad("sink").unwrap(), after, length);
            }
            Stall::Encoder => {
                let encoder = pipeline.by_name("encoder").unwrap();
                stall_once(&encoder.static_pad("sink").unwrap(), after, length);
            }
            Stall::AudioProducer => {
                let start = Instant::now();
                let silent = after..after + length;
                microphone.static_pad("src").unwrap().add_probe(
                    gst::PadProbeType::BUFFER,
                    move |_, _| {
                        if silent.contains(&start.elapsed()) {
                            gst::PadProbeReturn::Drop
                        } else {
                            gst::PadProbeReturn::Ok
                        }
                    },
                );
            }
            Stall::AudioFailure => {
                let start = Instant::now();
                microphone.static_pad("src").unwrap().add_probe(
                    gst::PadProbeType::BUFFER,
                    move |pad, info| {
                        if start.elapsed() < after {
                            return gst::PadProbeReturn::Ok;
                        }
                        reject_capture(
                            pad,
                            info,
                            gst::StreamError::Failed,
                            "Disconnected: Connection terminated",
                        )
                    },
                );
            }
        }
        std::thread::sleep(after + length + Duration::from_secs(4));
        let result = recording.finish();
        let probe = recording.probe();
        let dropped = dropped_frames(&recording.pipeline, &recording.counters);
        let (mut logs, mut media_lost) = (Vec::new(), None);
        for event in recording.events.try_iter() {
            match event {
                RecorderEvent::Log { message, .. } => logs.push(message),
                RecorderEvent::MediaLost { message, .. } => media_lost = Some(message),
                _ => {}
            }
        }
        StallOutcome {
            result,
            holes: audio_holes(&probe),
            audio_ends: probe["streams"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|stream| stream["codec_type"] == "audio")
                .map(|stream| {
                    let seconds = |key: &str| stream[key].as_str().unwrap().parse::<f64>().unwrap();
                    seconds("start_time") + seconds("duration")
                })
                .collect(),
            video_end: end_seconds(&probe, "h264"),
            missing_video: missing_video(&probe),
            dropped,
            logs,
            media_lost,
        }
    }

    #[test]
    fn a_stalled_file_write_loses_nothing_while_the_write_buffer_lasts() {
        let outcome = record_through(Stall::Write, 6.0, |_| {});
        outcome.result.as_ref().unwrap();
        assert_eq!(outcome.holes, [0.0, 0.0], "audio lost to the write stall");
        assert!(
            outcome.missing_video < 0.05 && outcome.dropped == 0,
            "{}s of video, {} frames lost to the write stall",
            outcome.missing_video,
            outcome.dropped
        );
        assert_eq!(outcome.media_lost, None);
    }

    // Past the write buffer every track waits for the disk, as without it.
    // The loss is bounded by the stall and reported as it was recorded.
    #[test]
    fn a_write_stall_longer_than_the_write_buffer_reports_what_it_cost() {
        let outcome = record_through(Stall::Write, 6.0, |pipeline| {
            pipeline
                .by_name(WRITE_QUEUE)
                .unwrap()
                .set_property("max-size-bytes", 16u32 << 10);
        });
        outcome.result.as_ref().unwrap();
        assert_reports_its_losses(&outcome);
        // Audio and video each wait about four seconds for the disk.
        for hole in &outcome.holes {
            assert!((1.0..2.5).contains(hole), "{:?}", outcome.holes);
        }
        assert!(
            (1.0..2.5).contains(&outcome.missing_video),
            "{}",
            outcome.missing_video
        );
    }

    #[test]
    fn an_encoder_stall_loses_its_video_and_reports_the_audio_it_cost() {
        let outcome = record_through(Stall::Encoder, 6.0, |_| {});
        outcome.result.as_ref().unwrap();
        assert_reports_its_losses(&outcome);
        // Audio waits as long as its queue lasts, then its source overwrites it.
        for hole in &outcome.holes {
            assert!((1.0..2.5).contains(hole), "{:?}", outcome.holes);
        }
        assert!(
            (5.5..6.5).contains(&outcome.missing_video),
            "{}",
            outcome.missing_video
        );
    }

    #[test]
    fn a_stalled_audio_source_costs_only_its_own_audio() {
        let outcome = record_through(Stall::AudioProducer, 2.0, |_| {});
        outcome.result.as_ref().unwrap();
        assert_reports_its_losses(&outcome);
        assert_eq!(
            outcome.holes[0], 0.0,
            "system audio waited for the microphone"
        );
        assert!(
            (1.9..2.1).contains(&outcome.holes[1]),
            "{:?}",
            outcome.holes
        );
        assert!(
            outcome.missing_video < 0.05 && outcome.dropped == 0,
            "{}s of video, {} frames lost waiting for the microphone",
            outcome.missing_video,
            outcome.dropped
        );
    }

    // Video waits for audio only as long as audio waits for video, then
    // drops at capture instead of building up encoded frames.
    #[test]
    fn an_audio_source_stalled_past_the_video_limit_reports_the_video_it_cost() {
        let most = Arc::new(AtomicU64::new(0));
        let level = most.clone();
        let outcome = record_through(Stall::AudioProducer, 6.0, |pipeline| {
            let held = pipeline.by_name(HELD_QUEUE).unwrap();
            held.connect("overrun", false, move |values| {
                let queue = values[0].get::<gst::Element>().unwrap();
                let frames = queue.property::<u32>("current-level-buffers");
                level.fetch_max(frames.into(), Ordering::SeqCst);
                None
            });
        });
        outcome.result.as_ref().unwrap();
        // The queue's time level spans the frames dropped meanwhile, so count
        // the frames it holds: the limit's worth at 30 fps.
        let limit = (HELD_VIDEO_LIMIT + gst::ClockTime::SECOND).seconds() * 30;
        let most = most.load(Ordering::SeqCst);
        assert!(
            (limit..=limit + 1).contains(&most),
            "{most} frames waited for the microphone"
        );
        assert_reports_its_losses(&outcome);
        assert!(
            (5.9..6.1).contains(&outcome.holes[1]),
            "{:?}",
            outcome.holes
        );
        assert!(
            (1.5..2.5).contains(&outcome.missing_video),
            "{}",
            outcome.missing_video
        );
    }

    #[test]
    fn a_failed_audio_source_ends_its_track_and_the_recording_continues() {
        let outcome = record_through(Stall::AudioFailure, 0.0, |_| {});
        outcome.result.as_ref().unwrap();
        assert!(
            outcome.missing_video < 0.05 && outcome.dropped == 0,
            "{}s of video, {} frames lost",
            outcome.missing_video,
            outcome.dropped
        );
        assert!((outcome.audio_ends[0] - outcome.video_end).abs() < 0.1);
        assert!(
            (1.8..2.3).contains(&outcome.audio_ends[1]),
            "the microphone ends at {}s",
            outcome.audio_ends[1]
        );
        let message = outcome.media_lost.unwrap();
        assert!(
            message.contains("microphone stopped at 2.")
                && message.contains("Disconnected: Connection terminated")
                && message.contains("so its track ends there"),
            "{message}"
        );
        assert!(!message.contains("system audio"), "{message}");
        assert!(
            outcome
                .logs
                .iter()
                .any(|log| log.contains("microphone stopped at")),
            "{:?}",
            outcome.logs
        );
    }

    // Audio captured before a pause but arriving after it is dropped by
    // design. A source that delivers late makes that drop longer than a
    // lost-audio hole. The source itself loses no audio, so any audio
    // reported lost was dropped by pausing. Video frames a starved encoder
    // drops are real losses, reported as such.
    #[test]
    fn audio_dropped_by_pausing_is_not_reported_lost() {
        let mut recording = TestRecording::start_with_audio(vec![(
            "microphone",
            lagging_audio("microphone", Duration::from_millis(100)),
        )]);
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(700));
            recording.control(true);
            std::thread::sleep(Duration::from_millis(450));
            recording.control(false);
        }
        std::thread::sleep(Duration::from_millis(1200));
        recording.finish().unwrap();
        recording.probe();
        for event in recording.events.try_iter() {
            match event {
                RecorderEvent::MediaLost { message, .. } => {
                    assert!(!message.contains(" of audio"), "{message}")
                }
                RecorderEvent::Log { message, .. } => {
                    assert!(!message.contains(" lost "), "{message}")
                }
                _ => {}
            }
        }
    }

    // Stops a recording `after` something in it got stuck at two seconds.
    // Returns the finished recording, its result and how long after stop
    // run() returned. Like pulsesrc, the audio sources' streaming threads
    // block on full queues.
    fn stop_while_stuck(
        after: Duration,
        stuck: impl FnOnce(&gst::Pipeline),
    ) -> (TestRecording, Result<()>, Duration) {
        let mut recording = TestRecording::start_with_audio(vec![
            ("system audio", delayed_audio(gst::ClockTime::ZERO)),
            ("microphone", delayed_audio(gst::ClockTime::ZERO)),
        ]);
        stuck(&recording.pipeline);
        std::thread::sleep(Duration::from_secs(2) + after);
        let stopped = Instant::now();
        let result = recording.finish();
        let returned = recording.returned.lock().unwrap().unwrap() - stopped;
        (recording, result, returned)
    }

    // Stopped once the audio queues are full, so the audio sources are
    // blocked, holding the stream locks that EOS needs.
    #[test]
    fn a_stuck_video_encoder_at_stop_still_finalizes_the_movie() {
        // Stuck past finalization; teardown waits for it to return.
        let (recording, result, returned) = stop_while_stuck(Duration::from_secs(6), |pipeline| {
            let encoder = pipeline.by_name("encoder").unwrap();
            stall_once(
                &encoder.static_pad("sink").unwrap(),
                Duration::from_secs(2),
                Duration::from_secs(20),
            );
        });
        result.unwrap();
        assert!(
            (FINALIZING..FINALIZING + Duration::from_secs(2)).contains(&returned),
            "finalized {returned:?} after stop"
        );
        let probe = recording.probe();
        let video = end_seconds(&probe, "h264");
        assert!((1.8..2.4).contains(&video), "video ends at {video}s");
        // Audio queued before its sources blocked made it into the movie.
        let audio = end_seconds(&probe, "aac");
        assert!(audio > 5.9, "audio ends at {audio}s");
        let (mut ending, mut lost) = (false, None);
        for event in recording.events.try_iter() {
            match event {
                RecorderEvent::Log { message, .. } => {
                    ending |= message.contains("ending the video track")
                }
                RecorderEvent::MediaLost { message, .. } => lost = Some(message),
                _ => {}
            }
        }
        assert!(ending);
        assert!(lost.is_some_and(|message| message.contains("the video ends early")),);
    }

    // Video waiting for the disk is not stuck, so it is never cut.
    #[test]
    fn a_disk_stall_at_stop_never_cuts_the_waiting_video() {
        let (recording, result, _) = stop_while_stuck(Duration::from_secs(1), |pipeline| {
            pipeline
                .by_name(WRITE_QUEUE)
                .unwrap()
                .set_property("max-size-bytes", 16u32 << 10);
            let sink = &pipeline_element(pipeline, "filesink")[0];
            stall_once(
                &sink.static_pad("sink").unwrap(),
                Duration::from_secs(2),
                Duration::from_secs(14),
            );
        });
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("finalization timed out"),
            "{error}"
        );
        for event in recording.events.try_iter() {
            if let RecorderEvent::Log { message, .. } = event {
                assert!(!message.contains("ending the video track"), "{message}");
            }
        }
    }

    // The microphone's producer inside its ring-buffer bin.
    fn producer(pipeline: &gst::Pipeline, name: &str) -> gst::Element {
        pipeline
            .by_name(name)
            .unwrap()
            .downcast::<gst::Bin>()
            .unwrap()
            .by_name("producer")
            .unwrap()
    }

    // The part of a stall spent paused is not in the movie, and the rest is
    // lost audio.
    #[test]
    fn audio_lost_across_a_pause_counts_only_recorded_time() {
        let mut recording =
            TestRecording::start_with_audio(vec![("microphone", ring_buffer_audio("microphone"))]);
        let start = Instant::now();
        let silent = Duration::from_secs(2)..Duration::from_millis(4500);
        producer(&recording.pipeline, "microphone")
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                if silent.contains(&start.elapsed()) {
                    gst::PadProbeReturn::Drop
                } else {
                    gst::PadProbeReturn::Ok
                }
            });
        std::thread::sleep(Duration::from_millis(2800));
        recording.control(true);
        std::thread::sleep(Duration::from_millis(700));
        recording.control(false);
        std::thread::sleep(Duration::from_secs(3));
        recording.finish().unwrap();
        let hole = audio_holes(&recording.probe())[0];
        assert!((1.6..2.0).contains(&hole), "the movie misses {hole}s");
        let message = recording
            .events
            .try_iter()
            .find_map(|event| match event {
                RecorderEvent::MediaLost { message, .. } => Some(message),
                _ => None,
            })
            .expect("no media loss reported");
        let reported: f64 = message
            .split_once("microphone is missing ")
            .and_then(|(_, rest)| rest.split_once(" s")?.0.parse().ok())
            .unwrap();
        assert!(
            (reported - hole).abs() < 0.05,
            "reported {reported}s, the movie misses {hole}s"
        );
    }

    // Audio lost between buffers with these capture times, in seconds, with
    // the timeline's pauses as (start, end), and a pause still going on.
    fn lost_between(buffers: &[(f64, f64)], pauses: &[(f64, f64)], paused_at: Option<f64>) -> f64 {
        gst::init().unwrap();
        let time = |seconds: f64| gst::ClockTime::from_nseconds((seconds * 1e9) as u64);
        let counters = Arc::new(Counters::default());
        {
            let mut timeline = counters.timeline.lock().unwrap();
            for &(start, end) in pauses {
                timeline.pause(time(start));
                timeline.resume(time(end));
            }
            if let Some(at) = paused_at {
                timeline.pause(time(at));
            }
        }
        let pipeline = PipelineGuard(gst::Pipeline::new());
        let source = element("appsrc").unwrap();
        source.set_property_from_str("format", "time");
        source.set_property("caps", gst::Caps::new_empty_simple("audio/x-raw"));
        let sink = element("fakesink").unwrap();
        sink.set_property("sync", false);
        pipeline.0.add_many([&source, &sink]).unwrap();
        source.link(&sink).unwrap();
        let lost = Arc::new(AtomicU64::new(0));
        count_lost_audio(&source, lost.clone(), counters);
        pipeline.0.set_state(gst::State::Playing).unwrap();
        for &(start, end) in buffers {
            let mut buffer = gst::Buffer::new();
            let buffer_mut = buffer.get_mut().unwrap();
            buffer_mut.set_pts(time(start));
            buffer_mut.set_duration(time(end - start));
            assert_eq!(
                source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
                gst::FlowReturn::Ok
            );
        }
        let _ = source.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
        let ended = pipeline.0.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        assert!(ended.is_some_and(|message| message.type_() == gst::MessageType::Eos));
        lost.load(Ordering::Relaxed) as f64 / 1e9
    }

    // Capture timestamps decide, not when buffers arrive: a source that
    // delivers late still has its gap inside the pause.
    #[test]
    fn only_the_unpaused_part_of_a_capture_gap_is_lost() {
        let near = |lost: f64, expected: f64| (lost - expected).abs() < 1e-6;
        // Inside a pause from 2 s to 4 s.
        let lost = lost_between(&[(2.04, 2.05), (3.95, 3.96)], &[(2.0, 4.0)], None);
        assert!(near(lost, 0.0), "{lost}");
        // From 1.5 s to 4.5 s, half a second on each side of it.
        let lost = lost_between(&[(1.49, 1.5), (4.5, 4.51)], &[(2.0, 4.0)], None);
        assert!(near(lost, 1.0), "{lost}");
        // Into a pause that has not ended.
        let lost = lost_between(&[(1.49, 1.5), (3.0, 3.01)], &[], Some(2.0));
        assert!(near(lost, 0.5), "{lost}");
        // Shorter than a hole.
        let lost = lost_between(&[(1.0, 1.01), (1.02, 1.03)], &[], None);
        assert!(near(lost, 0.0), "{lost}");
    }

    // The queue at the end of an audio source's branch, in front of the
    // muxer.
    fn audio_branch_end(pipeline: &gst::Pipeline, source: &str) -> gst::Element {
        let mut element = pipeline.by_name(source).unwrap();
        while !matches!(element.factory(), Some(factory) if factory.name() == "queue") {
            element = element
                .static_pad("src")
                .unwrap()
                .peer()
                .unwrap()
                .parent_element()
                .unwrap();
        }
        element
    }

    // A source that reports a fatal error but leaves its branch open still
    // ends its track, so the muxer does not wait for it. Video waits for an
    // audio track for up to HELD_VIDEO_LIMIT before any is dropped, so the
    // track has to end at the muxer sooner. The movie's video is no measure
    // of this: a starved encoder drops frames at capture whether or not a
    // source failed.
    #[test]
    fn a_failed_audio_source_that_keeps_its_branch_open_still_ends_its_track() {
        let mut recording = TestRecording::start_with_audio(vec![
            ("system audio", ring_buffer_audio("system-audio")),
            ("microphone", ring_buffer_audio("microphone")),
        ]);
        let start = Instant::now();
        let failed = Arc::new(Mutex::new(None));
        let failing = failed.clone();
        producer(&recording.pipeline, "microphone")
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
                if start.elapsed() < Duration::from_secs(2) {
                    return gst::PadProbeReturn::Ok;
                }
                let mut failing = failing.lock().unwrap();
                if failing.is_none() {
                    *failing = Some(Instant::now());
                    let source = pad.parent_element().unwrap();
                    gst::element_error!(source, gst::ResourceError::Read, ("device unplugged"));
                }
                gst::PadProbeReturn::Drop
            });
        let ended = Arc::new(Mutex::new(None));
        let ending = ended.clone();
        audio_branch_end(&recording.pipeline, "microphone")
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                if info
                    .event()
                    .is_some_and(|event| event.type_() == gst::EventType::Eos)
                {
                    ending.lock().unwrap().get_or_insert_with(Instant::now);
                }
                gst::PadProbeReturn::Ok
            });
        std::thread::sleep(Duration::from_secs(8));
        recording.finish().unwrap();
        recording.probe();
        let waited = ended.lock().unwrap().unwrap() - failed.lock().unwrap().unwrap();
        assert!(
            waited < Duration::from_nanos(HELD_VIDEO_LIMIT.nseconds()),
            "the muxer waited {waited:?} for the failed microphone"
        );
        let message = recording
            .events
            .try_iter()
            .find_map(|event| match event {
                RecorderEvent::MediaLost { message, .. } => Some(message),
                _ => None,
            })
            .unwrap();
        assert!(message.contains("microphone stopped at 2."), "{message}");
    }

    // Only a source going away is recoverable. Any other audio error fails
    // the recording, as before.
    #[test]
    fn an_audio_encoder_error_fails_the_recording() {
        let mut recording =
            TestRecording::start_with_audio(vec![("microphone", ring_buffer_audio("microphone"))]);
        let encoder = pipeline_element(&recording.pipeline, "avenc_aac")[0].clone();
        std::thread::sleep(Duration::from_secs(1));
        gst::element_error!(encoder, gst::LibraryError::Encode, ("encoder failed"));
        let at = Instant::now();
        let error = recording.finish().unwrap_err();
        assert!(error.to_string().contains("encoder failed"), "{error}");
        assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    }

    // What the recording reported matches what its movie is missing.
    fn assert_reports_its_losses(outcome: &StallOutcome) {
        let message = outcome
            .media_lost
            .as_deref()
            .expect("no media loss reported");
        assert_eq!(
            outcome.dropped > 0,
            message.contains(&format!("{} video frames were dropped", outcome.dropped)),
            "{message}"
        );
        // Dropped frames account for the missing video, give or take a
        // frame at either edge of each gap.
        assert!(
            (outcome.dropped as f64 / 30.0 - outcome.missing_video).abs() < 0.15,
            "{} dropped frames, {}s of video missing",
            outcome.dropped,
            outcome.missing_video
        );
        for (track, hole) in ["system audio", "microphone"].iter().zip(&outcome.holes) {
            let reported = outcome.reported(track);
            assert!(
                (reported - hole).abs() < 0.05,
                "{track}: reported {reported}s lost, movie misses {hole}s: {message}"
            );
            assert_eq!(
                *hole > 0.0,
                outcome
                    .logs
                    .iter()
                    .any(|log| log.starts_with(&format!("capture-engine: {track} lost "))),
                "{track}: {:?}",
                outcome.logs
            );
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
        let partial = recording.session.output_path.with_extension("partial.mp4");
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
        // mp4mux holds its audio until the next frame arrives.
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
        // mp4mux before the pause would leave a hole.
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
        movie.add_audio("test audio", &silent, &silent, Arc::default());
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
        // Audio dropped while paused is not lost media.
        for event in recording.events.try_iter() {
            match event {
                RecorderEvent::MediaLost { message, .. } => panic!("{message}"),
                RecorderEvent::Log { message, .. } => {
                    assert!(!message.contains(" lost "), "{message}")
                }
                _ => {}
            }
        }
    }

    // An idle screen sends a frame each keepalive. A short pause that swallows
    // one leaves almost two keepalives between the frames around it, and mp4mux
    // holds audio until the frame after those. The audio queue must absorb
    // that wait: a full queue blocks pulsesrc, which then drops audio.
    #[test]
    fn audio_waiting_for_idle_video_across_a_pause_never_fills_its_queue() {
        let mut recording = TestRecording::start_idle(2);
        let full = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for queue in recording.pipeline.iterate_elements().into_iter().flatten() {
            let audio = queue
                .factory()
                .is_some_and(|factory| factory.name() == "queue")
                && ![CAPTURE_QUEUE, HELD_QUEUE, WRITE_QUEUE].contains(&queue.name().as_str());
            if audio {
                let full = full.clone();
                queue.connect("overrun", false, move |_| {
                    full.store(true, Ordering::SeqCst);
                    None
                });
            }
        }
        let arrived = Arc::new(AtomicU64::new(0));
        let latest = arrived.clone();
        let pipeline = recording.pipeline.clone();
        recording
            .pipeline
            .by_name("video")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                let now = pipeline.current_running_time().unwrap_or_default();
                latest.store(now.nseconds(), Ordering::SeqCst);
                gst::PadProbeReturn::Ok
            });
        let keepalive = Duration::from_nanos(KEEPALIVE.nseconds());
        for _ in 0..2 {
            // Pause three quarters of a keepalive after a frame and resume a
            // quarter after the next one, the worst case short of no pause.
            let frame = arrived.load(Ordering::SeqCst);
            let waiting = Instant::now();
            while arrived.load(Ordering::SeqCst) == frame {
                assert!(waiting.elapsed() < keepalive * 3, "idle video stopped");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(keepalive * 3 / 4);
            recording.control(true);
            std::thread::sleep(keepalive / 2);
            recording.control(false);
            std::thread::sleep(keepalive * 3);
        }
        recording.finish().unwrap();
        assert!(
            !full.load(Ordering::SeqCst),
            "audio queue filled while mp4mux waited for idle video"
        );
        recording.probe();
    }

    fn test_session() -> RecordingSession {
        static ID: AtomicU64 = AtomicU64::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        RecordingSession {
            id,
            output_path: std::env::temp_dir().join(format!(
                "wrec-linux-record-test-{}-{id}.mp4",
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

    // record() on its own thread. Every wait on it has a deadline, so an engine
    // failure fails the test instead of hanging it. Dropping it stops the
    // recording and waits as long as the daemon waits for a worker to exit.
    struct RecordThread {
        stop: watch::Sender<bool>,
        returned: mpsc::Receiver<Result<()>>,
    }

    impl RecordThread {
        fn spawn(
            capture: CaptureInput,
            session: &RecordingSession,
            codec: Codec,
            events: mpsc::Sender<RecorderEvent>,
        ) -> Self {
            let (commands, receiver) = mpsc::sync_channel(1);
            let (stop, stopped) = watch::channel(false);
            let (done, returned) = mpsc::channel();
            let session = session.clone();
            std::thread::spawn(move || {
                let _commands = commands;
                let result = record(
                    &capture,
                    &session,
                    &silent_settings(codec),
                    &events,
                    receiver,
                    &stopped,
                    &|_| {},
                );
                let _ = done.send(result);
            });
            Self { stop, returned }
        }

        fn assert_running(&self) {
            match self.returned.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {}
                outcome => panic!("the recording ended early: {outcome:?}"),
            }
        }

        fn finish(&self) -> Result<()> {
            self.stop.send_replace(true);
            self.returned
                .recv_timeout(crate::worker::EXIT_DEADLINE)
                .unwrap_or_else(|error| panic!("the recording did not return after stop: {error}"))
        }
    }

    impl Drop for RecordThread {
        fn drop(&mut self) {
            self.stop.send_replace(true);
            let _ = self.returned.recv_timeout(crate::worker::EXIT_DEADLINE);
        }
    }

    // The test's appsrc once the recording plays it.
    fn playing_source(
        source: &Mutex<Option<gst::Element>>,
        recording: &RecordThread,
    ) -> gst::Element {
        let start = Instant::now();
        loop {
            recording.assert_running();
            let created = source.lock().unwrap().clone();
            if let Some(appsrc) =
                created.filter(|appsrc| appsrc.current_state() == gst::State::Playing)
            {
                return appsrc;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the capture source did not play within 10s"
            );
            std::thread::sleep(Duration::from_millis(10));
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
        let partial = session.output_path.with_extension("partial.mp4");
        let (events, received) = mpsc::channel();
        let recording = RecordThread::spawn(capture, &session, codec, events);
        let appsrc = playing_source(&source, &recording);
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
        // mp4mux writes a fragment when the next keyframe reaches it, so wait
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
        let result = recording.finish();
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

    // Records white edges around a black picture and returns the movie.
    fn record_framed_picture(codec: Codec, width: i32, height: i32) -> RecordingSession {
        gst::init().unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "BGRx")
            .field("width", width)
            .field("height", height)
            .field("framerate", gst::Fraction::new(0, 1))
            .field("max-framerate", gst::Fraction::new(10, 1))
            .build();
        let mut picture = vec![0u8; (width * height * 4) as usize];
        for (index, pixel) in picture.chunks_mut(4).enumerate() {
            let (x, y) = (index as i32 % width, index as i32 / width);
            if x.min(width - 1 - x).min(y).min(height - 1 - y) < EDGE {
                pixel.fill(255);
            }
        }
        let source = Arc::new(Mutex::new(None::<gst::Element>));
        let created = source.clone();
        let capture = CaptureInput::Element(Box::new(move || {
            let source = element("appsrc").unwrap();
            source.set_property("is-live", true);
            source.set_property_from_str("format", "time");
            source.set_property("caps", &caps);
            *created.lock().unwrap() = Some(source.clone());
            source
        }));
        let session = test_session();
        let (events, _received) = mpsc::channel();
        let recording = RecordThread::spawn(capture, &session, codec, events);
        let appsrc = playing_source(&source, &recording);
        let origin = appsrc.current_running_time().unwrap_or_default();
        for frame in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            let mut buffer = gst::Buffer::from_slice(picture.clone());
            let buffer_mut = buffer.get_mut().unwrap();
            buffer_mut.set_pts(origin + gst::ClockTime::from_mseconds(100 * frame));
            assert_eq!(
                appsrc.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
                gst::FlowReturn::Ok
            );
        }
        let pushed = Instant::now();
        while appsrc.property::<u64>("current-level-buffers") > 0 {
            recording.assert_running();
            assert!(
                pushed.elapsed() < Duration::from_secs(5),
                "the recording took no frame for 5s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        recording.finish().unwrap();
        session
    }

    const EDGE: i32 = 16;

    // The pictures a player shows, decoded with FFmpeg's default cropping.
    fn decoded_pictures(movie: &Path) -> Vec<(usize, usize, Vec<u8>)> {
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(movie)
            .args(["-map", "0:v:0", "-vsync", "0", "-pix_fmt", "gray"])
            .args(["-c:v", "pgm", "-f", "image2pipe", "-"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut pictures = Vec::new();
        let mut rest = &output.stdout[..];
        while !rest.is_empty() {
            // "P5\n<width> <height>\n255\n" followed by the pixels.
            let mut fields = Vec::new();
            while fields.len() < 4 {
                let end = rest.iter().position(u8::is_ascii_whitespace).unwrap();
                fields.push(String::from_utf8(rest[..end].to_vec()).unwrap());
                rest = &rest[end + 1..];
            }
            let width: usize = fields[1].parse().unwrap();
            let height: usize = fields[2].parse().unwrap();
            pictures.push((width, height, rest[..width * height].to_vec()));
            rest = &rest[width * height..];
        }
        pictures
    }

    // Boxes in the movie's final index, down to each video sample entry. A
    // finished movie keeps its first index inside its media data box as
    // "hoov", where this walk does not look, so a partial movie's first index
    // goes unchecked.
    fn video_boxes(movie: &Path) -> Vec<(String, Vec<u8>)> {
        fn children(data: &[u8]) -> Vec<(&str, &[u8])> {
            let mut boxes = Vec::new();
            let mut rest = data;
            while rest.len() >= 8 {
                let size = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
                let size = if size == 0 { rest.len() } else { size };
                if size < 8 || size > rest.len() {
                    break;
                }
                let name = std::str::from_utf8(&rest[4..8]).unwrap_or("?");
                boxes.push((name, &rest[8..size]));
                rest = &rest[size..];
            }
            boxes
        }
        fn walk(name: &str, body: &[u8], found: &mut Vec<(String, Vec<u8>)>) {
            let body = match name {
                "moov" | "trak" | "tapt" | "mdia" | "minf" | "stbl" => body,
                // A full box with an entry count.
                "stsd" => &body[8..],
                // The fields of a visual sample entry come before its boxes.
                "avc1" | "avc3" | "hvc1" | "hev1" => &body[78..],
                _ => return,
            };
            for (child, payload) in children(body) {
                found.push((child.to_string(), payload.to_vec()));
                walk(child, payload, found);
            }
        }
        let data = std::fs::read(movie).unwrap();
        let mut found = Vec::new();
        for (name, body) in children(&data) {
            walk(name, body, &mut found);
        }
        found
    }

    fn assert_records_every_pixel(codec: Codec, width: i32, height: i32) {
        let session = record_framed_picture(codec, width, height);
        let movie = &session.output_path;
        let file_type = std::fs::read(movie).unwrap()[4..12].to_vec();
        let boxes = video_boxes(movie);
        let pictures = decoded_pictures(movie);
        let _ = std::fs::remove_file(movie);
        // Players crop to a clean aperture ("clap") or a track aperture
        // ("tapt"). FFmpeg 8.0 applies clap when decoding; FFmpeg 7.0 does not.
        let size = (width as u32, height as u32);
        for (name, payload) in &boxes {
            let field = |index: usize| {
                u32::from_be_bytes(payload[index * 4..index * 4 + 4].try_into().unwrap())
            };
            let aperture = match name.as_str() {
                // Width and height as fractions.
                "clap" => (field(0) / field(1), field(2) / field(3)),
                // A full box header, then 16.16 fixed-point width and height.
                "clef" | "prof" | "enof" => (field(1) >> 16, field(2) >> 16),
                _ => continue,
            };
            assert_eq!(aperture, size, "{name} crops the movie's picture");
        }
        assert!(
            boxes
                .iter()
                .any(|(name, _)| name == "avcC" || name == "hvcC"),
            "found no video sample entry in {boxes:?}"
        );
        assert!(!pictures.is_empty());
        for (shown_width, shown_height, pixels) in pictures {
            let shown = (shown_width as u32, shown_height as u32);
            assert_eq!(shown, size, "the player shows a different picture");
            let luma = |x: usize, y: usize| pixels[y * shown_width + x];
            let (right, bottom) = (shown_width - 1, shown_height - 1);
            let middle = (shown_width / 2, shown_height / 2);
            let edges = [
                (0, middle.1),
                (right, middle.1),
                (middle.0, 0),
                (middle.0, bottom),
                (0, 0),
                (right, bottom),
            ];
            for (x, y) in edges {
                assert!(luma(x, y) > 200, "the edge at {x},{y} is missing");
            }
            for (x, y) in [
                (EDGE as usize + 2, middle.1),
                (right - EDGE as usize - 2, middle.1),
                middle,
            ] {
                assert!(luma(x, y) < 50, "the picture at {x},{y} moved");
            }
        }

        // Linux names recordings ".mp4", so the file must say it is MP4.
        assert_eq!(
            String::from_utf8_lossy(&file_type),
            "ftypmp42",
            "the movie's file type"
        );
    }

    // qtmux crops these sizes to a guessed TV aspect ratio; see movie_mux.
    #[test]
    fn h264_recordings_keep_every_pixel_at_tv_like_sizes() {
        assert_records_every_pixel(Codec::H264, 800, 528);
    }

    #[test]
    fn hevc_recordings_keep_every_pixel_at_tv_like_sizes() {
        assert_records_every_pixel(Codec::Hevc, 800, 528);
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
            let finished = at.elapsed();
            let returned = recording.returned.lock().unwrap().unwrap() - at;
            assert!(
                finished < Duration::from_secs(1),
                "paused {paused}, waiting for audio {}: run returned after {returned:?}, \
                 the worker finished after {finished:?}",
                audio == gst::ClockTime::MAX
            );
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
