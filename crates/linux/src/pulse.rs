// Audio capture straight from libpulse, which PulseAudio and pipewire-pulse
// both serve, into an appsrc.

use crate::backend;
use domain::Result;
use gstreamer::{self as gst, glib, prelude::*};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Duration,
};

type Mainloop = c_void;
type Context = c_void;
type Stream = c_void;
type Operation = c_void;
type ContextCallback = unsafe extern "C" fn(*mut Context, *mut c_void);
type StreamCallback = unsafe extern "C" fn(*mut Stream, *mut c_void);
type ReadCallback = unsafe extern "C" fn(*mut Stream, usize, *mut c_void);
type SuccessCallback = unsafe extern "C" fn(*mut Stream, c_int, *mut c_void);

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

#[repr(C)]
struct TimingInfo {
    timestamp: libc::timeval,
    synchronized_clocks: c_int,
    sink_usec: u64,
    source_usec: u64,
    transport_usec: u64,
    playing: c_int,
    write_index_corrupt: c_int,
    write_index: i64,
    read_index_corrupt: c_int,
    read_index: i64,
    configured_sink_usec: u64,
    configured_source_usec: u64,
    since_underrun: i64,
}

macro_rules! library {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        struct Library {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl Library {
            fn open() -> std::result::Result<Self, String> {
                let handle = unsafe { libc::dlopen(c"libpulse.so.0".as_ptr(), libc::RTLD_NOW) };
                if handle.is_null() {
                    return Err("libpulse.so.0 could not be loaded".into());
                }
                Ok(Self {
                    $($name: unsafe {
                        let symbol = libc::dlsym(
                            handle,
                            concat!("pa_", stringify!($name), "\0").as_ptr().cast(),
                        );
                        if symbol.is_null() {
                            return Err(concat!("libpulse has no pa_", stringify!($name)).into());
                        }
                        std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                    },)*
                })
            }
        }
    };
}

library! {
    threaded_mainloop_new: fn() -> *mut Mainloop;
    threaded_mainloop_free: fn(*mut Mainloop);
    threaded_mainloop_start: fn(*mut Mainloop) -> c_int;
    threaded_mainloop_stop: fn(*mut Mainloop);
    threaded_mainloop_lock: fn(*mut Mainloop);
    threaded_mainloop_unlock: fn(*mut Mainloop);
    threaded_mainloop_in_thread: fn(*mut Mainloop) -> c_int;
    threaded_mainloop_get_api: fn(*mut Mainloop) -> *mut c_void;
    threaded_mainloop_set_name: fn(*mut Mainloop, *const c_char);
    context_new: fn(*mut c_void, *const c_char) -> *mut Context;
    context_set_state_callback: fn(*mut Context, Option<ContextCallback>, *mut c_void);
    context_connect: fn(*mut Context, *const c_char, c_int, *const c_void) -> c_int;
    context_get_state: fn(*mut Context) -> c_int;
    context_disconnect: fn(*mut Context);
    context_unref: fn(*mut Context);
    context_errno: fn(*mut Context) -> c_int;
    strerror: fn(c_int) -> *const c_char;
    stream_new: fn(*mut Context, *const c_char, *const SampleSpec, *const c_void) -> *mut Stream;
    stream_set_state_callback: fn(*mut Stream, Option<StreamCallback>, *mut c_void);
    stream_set_read_callback: fn(*mut Stream, Option<ReadCallback>, *mut c_void);
    stream_connect_record: fn(*mut Stream, *const c_char, *const BufferAttr, c_int) -> c_int;
    stream_get_state: fn(*mut Stream) -> c_int;
    stream_peek: fn(*mut Stream, *mut *const c_void, *mut usize) -> c_int;
    stream_drop: fn(*mut Stream) -> c_int;
    stream_cork: fn(*mut Stream, c_int, Option<SuccessCallback>, *mut c_void) -> *mut Operation;
    stream_update_timing_info: fn(*mut Stream, Option<SuccessCallback>, *mut c_void) -> *mut Operation;
    stream_get_timing_info: fn(*mut Stream) -> *const TimingInfo;
    stream_disconnect: fn(*mut Stream) -> c_int;
    stream_unref: fn(*mut Stream);
    operation_unref: fn(*mut Operation);
}

fn library() -> Result<&'static Library> {
    static LIBRARY: OnceLock<std::result::Result<Library, String>> = OnceLock::new();
    LIBRARY.get_or_init(Library::open).as_ref().map_err(|error| {
        backend(format!(
            "{error}. System audio and the microphone need libpulse, which PulseAudio and pipewire-pulse serve."
        ))
    })
}

pub(crate) fn available() -> Result<()> {
    library().map(|_| ())
}

const CONTEXT_READY: c_int = 4;
const CONTEXT_FAILED: c_int = 5;
const CONTEXT_TERMINATED: c_int = 6;
const STREAM_READY: c_int = 2;
const STREAM_FAILED: c_int = 3;
const STREAM_TERMINATED: c_int = 4;
const CONTEXT_NOAUTOSPAWN: c_int = 1;
const STREAM_START_CORKED: c_int = 0x1;
const STREAM_ADJUST_LATENCY: c_int = 0x2000;
const SAMPLE_FLOAT32LE: c_int = 5;

pub(crate) const RATE: u32 = 48000;
pub(crate) const CHANNELS: u8 = 2;
const FRAME: usize = 4 * CHANNELS as usize;

// The server sends audio in fragments of this length, so capture latency.
const FRAGMENT: gst::ClockTime = gst::ClockTime::from_mseconds(10);

// How long a server can take to accept the connection and the stream.
const CONNECTING: Duration = Duration::from_secs(10);

fn bytes_of(time: gst::ClockTime) -> u32 {
    (time.nseconds() * RATE as u64 / 1_000_000_000) as u32 * FRAME as u32
}

fn time_of(bytes: i64) -> i64 {
    (bytes as i128 / FRAME as i128 * 1_000_000_000 / RATE as i128) as i64
}

// An appsrc fed by a libpulse record stream from `device` (the default
// source when None) on `server` (the default server when None). Fails if
// libpulse, the server or the device is unavailable. `buffer` is how much
// audio the server holds for this reader, and appsrc for downstream.
pub(crate) fn source(
    server: Option<&str>,
    device: Option<&str>,
    buffer: gst::ClockTime,
) -> Result<gst::Element> {
    let library = library()?;
    let source = crate::pipeline::element("appsrc")?;
    source.set_property(
        "caps",
        gst::Caps::builder("audio/x-raw")
            .field("format", "F32LE")
            .field("rate", RATE as i32)
            .field("channels", CHANNELS as i32)
            .field("layout", "interleaved")
            .build(),
    );
    source.set_property("is-live", true);
    source.set_property("format", gst::Format::Time);
    source.set_property("do-timestamp", false);
    source.set_property("block", true);
    source.set_property("max-bytes", bytes_of(buffer) as u64);
    source.set_property("min-latency", FRAGMENT.nseconds() as i64);
    source.set_property("max-latency", buffer.nseconds() as i64);
    let capture = Capture::connect(library, &source, server, device, buffer)?;
    let capture = Mutex::new((capture, false));
    // A live appsrc first asks for data when the pipeline plays.
    source.connect("need-data", false, move |_| {
        let mut capture = capture.lock().unwrap();
        if !capture.1 {
            capture.1 = true;
            capture.0.start();
        }
        None
    });
    Ok(source)
}

// Owns the libpulse thread and the reader it calls back into.
struct Capture {
    library: &'static Library,
    mainloop: *mut Mainloop,
    reader: *mut Reader,
}

// libpulse objects are used only under the mainloop lock or after its thread
// stopped.
unsafe impl Send for Capture {}

impl Capture {
    fn connect(
        library: &'static Library,
        source: &gst::Element,
        server: Option<&str>,
        device: Option<&str>,
        buffer: gst::ClockTime,
    ) -> Result<Self> {
        let server = server
            .map(|server| CString::new(server).map_err(backend))
            .transpose()?;
        let device = device
            .map(|device| CString::new(device).map_err(backend))
            .transpose()?;
        let mainloop = unsafe { (library.threaded_mainloop_new)() };
        if mainloop.is_null() {
            return Err(backend("libpulse could not create its mainloop"));
        }
        let connected = Arc::new((Mutex::new(None), Condvar::new()));
        let reader = Box::into_raw(Box::new(Reader {
            library,
            context: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            device,
            buffer,
            source: source.downgrade(),
            connected: Some(connected.clone()),
            closed: false,
            position: 0,
            stamps: Stamps::default(),
            held: Vec::new(),
            end: None,
            last_request: None,
            asking: 0,
            last_arrival: None,
            resumed: false,
        }));
        let capture = Self {
            library,
            mainloop,
            reader,
        };
        unsafe {
            (library.threaded_mainloop_set_name)(mainloop, c"wrec-pulse".as_ptr());
            let api = (library.threaded_mainloop_get_api)(mainloop);
            let context = (library.context_new)(api, c"wrec".as_ptr());
            if context.is_null() {
                return Err(backend("libpulse could not create a context"));
            }
            (*reader).context = context;
            (library.context_set_state_callback)(context, Some(context_state), reader.cast());
            if (library.context_connect)(
                context,
                server
                    .as_ref()
                    .map_or(std::ptr::null(), |server| server.as_ptr()),
                CONTEXT_NOAUTOSPAWN,
                std::ptr::null(),
            ) < 0
            {
                return Err(backend(format!(
                    "could not connect to the audio server: {}",
                    error_text(library, context)
                )));
            }
            if (library.threaded_mainloop_start)(mainloop) < 0 {
                return Err(backend("libpulse could not start its thread"));
            }
        }
        let (state, ready) = &*connected;
        let (state, timeout) = ready
            .wait_timeout_while(state.lock().unwrap(), CONNECTING, |state| state.is_none())
            .unwrap();
        match state.clone() {
            Some(Ok(())) => Ok(capture),
            Some(Err(error)) => Err(backend(error)),
            None if timeout.timed_out() => Err(backend(format!(
                "the audio server did not accept a recording stream within {} s",
                CONNECTING.as_secs()
            ))),
            None => unreachable!(),
        }
    }

    fn start(&self) {
        unsafe {
            (self.library.threaded_mainloop_lock)(self.mainloop);
            (*self.reader).start();
            (self.library.threaded_mainloop_unlock)(self.mainloop);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let parts = Parts(self.library, self.mainloop, self.reader);
        // The libpulse thread can't stop itself, and drops the source last
        // if it was pushing to it when the pipeline let go of it.
        if unsafe { (self.library.threaded_mainloop_in_thread)(self.mainloop) } != 0 {
            std::thread::spawn(move || parts.stop());
        } else {
            parts.stop();
        }
    }
}

struct Parts(&'static Library, *mut Mainloop, *mut Reader);

unsafe impl Send for Parts {}

impl Parts {
    fn stop(self) {
        let Parts(library, mainloop, reader) = self;
        unsafe {
            (library.threaded_mainloop_stop)(mainloop);
            (*reader).close();
            let reader = Box::from_raw(reader);
            if !reader.context.is_null() {
                (library.context_set_state_callback)(reader.context, None, std::ptr::null_mut());
                (library.context_disconnect)(reader.context);
                (library.context_unref)(reader.context);
            }
            drop(reader);
            (library.threaded_mainloop_free)(mainloop);
        }
    }
}

type Connected = Arc<(Mutex<Option<std::result::Result<(), String>>>, Condvar)>;

// Lives on the libpulse thread, or under its lock.
struct Reader {
    library: &'static Library,
    context: *mut Context,
    stream: *mut Stream,
    device: Option<CString>,
    buffer: gst::ClockTime,
    source: glib::WeakRef<gst::Element>,
    // Until the stream is ready or failed, where Capture::connect waits.
    connected: Option<Connected>,
    closed: bool,
    // Bytes of the stream so far, received or skipped, so the server's
    // index of the next byte.
    position: i64,
    stamps: Stamps,
    // Audio waiting for the server to say when it captured audio, whether
    // it lost any while audio stopped arriving, or where it lost some.
    held: Vec<(i64, gst::Buffer)>,
    // Where the last buffer pushed ended.
    end: Option<i64>,
    last_request: Option<gst::ClockTime>,
    // Questions on the way.
    asking: u32,
    last_arrival: Option<gst::ClockTime>,
    // Since audio stopped arriving for a while, until the server says
    // whether it lost any meanwhile.
    resumed: bool,
}

fn error_text(library: &Library, context: *mut Context) -> String {
    unsafe {
        CStr::from_ptr((library.strerror)((library.context_errno)(context)))
            .to_string_lossy()
            .into_owned()
    }
}

unsafe extern "C" fn context_state(context: *mut Context, reader: *mut c_void) {
    let reader = &mut *reader.cast::<Reader>();
    match (reader.library.context_get_state)(context) {
        CONTEXT_READY => reader.open_stream(),
        CONTEXT_FAILED | CONTEXT_TERMINATED => {
            let error = format!(
                "the audio server connection ended: {}",
                error_text(reader.library, context)
            );
            reader.fail(error);
        }
        _ => {}
    }
}

unsafe extern "C" fn stream_state(stream: *mut Stream, reader: *mut c_void) {
    let reader = &mut *reader.cast::<Reader>();
    match (reader.library.stream_get_state)(stream) {
        STREAM_READY => reader.report_connected(Ok(())),
        STREAM_FAILED | STREAM_TERMINATED => {
            let error = format!(
                "the audio server ended the recording stream: {}",
                error_text(reader.library, reader.context)
            );
            reader.fail(error);
        }
        _ => {}
    }
}

unsafe extern "C" fn stream_read(_: *mut Stream, _: usize, reader: *mut c_void) {
    (*reader.cast::<Reader>()).read();
}

unsafe extern "C" fn timing_updated(_: *mut Stream, success: c_int, reader: *mut c_void) {
    (*reader.cast::<Reader>()).timing_updated(success != 0);
}

// Questions on the way at most, as for a server that stopped answering.
const ASKING: u32 = 64;

// How long video waits for an audio track to start.
const FIRST_ACCOUNT: gst::ClockTime = crate::pipeline::HELD_VIDEO_LIMIT;

impl Reader {
    unsafe fn open_stream(&mut self) {
        let spec = SampleSpec {
            format: SAMPLE_FLOAT32LE,
            rate: RATE,
            channels: CHANNELS,
        };
        let stream = (self.library.stream_new)(
            self.context,
            c"wrec recording".as_ptr(),
            &spec,
            std::ptr::null(),
        );
        if stream.is_null() {
            let error = format!(
                "the audio server refused a recording stream: {}",
                error_text(self.library, self.context)
            );
            return self.fail(error);
        }
        self.stream = stream;
        let reader = (self as *mut Self).cast();
        (self.library.stream_set_state_callback)(stream, Some(stream_state), reader);
        (self.library.stream_set_read_callback)(stream, Some(stream_read), reader);
        let attributes = BufferAttr {
            maxlength: bytes_of(self.buffer),
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: bytes_of(FRAGMENT),
        };
        if (self.library.stream_connect_record)(
            stream,
            self.device
                .as_ref()
                .map_or(std::ptr::null(), |device| device.as_ptr()),
            &attributes,
            STREAM_START_CORKED | STREAM_ADJUST_LATENCY,
        ) < 0
        {
            let error = format!(
                "the audio server refused a recording stream: {}",
                error_text(self.library, self.context)
            );
            self.fail(error);
        }
    }

    fn report_connected(&mut self, result: std::result::Result<(), String>) {
        if let Some(connected) = self.connected.take() {
            *connected.0.lock().unwrap() = Some(result);
            connected.1.notify_all();
        }
    }

    // Before the pipeline plays, the error goes to Capture::connect; after
    // it, to the bus, which ends the track.
    unsafe fn fail(&mut self, error: String) {
        if self.connected.is_some() {
            return self.report_connected(Err(error));
        }
        if self.closed {
            return;
        }
        self.close();
        if let Some(source) = self.source.upgrade() {
            source.post_error_message(gst::error_msg!(gst::ResourceError::Read, ["{}", error]));
        }
    }

    unsafe fn close(&mut self) {
        self.closed = true;
        if !self.stream.is_null() {
            (self.library.stream_set_state_callback)(self.stream, None, std::ptr::null_mut());
            (self.library.stream_set_read_callback)(self.stream, None, std::ptr::null_mut());
            (self.library.stream_disconnect)(self.stream);
            (self.library.stream_unref)(self.stream);
            self.stream = std::ptr::null_mut();
        }
    }

    unsafe fn start(&mut self) {
        if self.closed || self.stream.is_null() {
            return;
        }
        let operation = (self.library.stream_cork)(self.stream, 0, None, std::ptr::null_mut());
        if !operation.is_null() {
            (self.library.operation_unref)(operation);
        }
        self.request_timing();
    }

    fn now(&self) -> Option<(gst::ClockTime, gst::ClockTime)> {
        let source = self.source.upgrade()?;
        let clock = source.clock()?;
        let now = clock.time()?;
        Some((now, now.saturating_sub(source.base_time()?)))
    }

    // Answers come in order, each after the audio sent before it, and more
    // than one can be on the way, up to ASKING for a server that stopped.
    unsafe fn request_timing(&mut self) {
        if self.closed || self.asking >= ASKING {
            return;
        }
        let operation = (self.library.stream_update_timing_info)(
            self.stream,
            Some(timing_updated),
            (self as *mut Self).cast(),
        );
        if operation.is_null() {
            return;
        }
        (self.library.operation_unref)(operation);
        self.asking += 1;
        self.last_request = self.now().map(|(_, running)| running);
    }

    // The server's write index, when it captured the audio there, and its
    // read index. A read index past the audio received is audio the server
    // skipped, which pipewire-pulse does for a reader that fell a buffer
    // behind; the reply comes after the audio sent before it. libpulse
    // already took off what it holds, and read() takes all it holds.
    unsafe fn timing_updated(&mut self, success: bool) {
        self.asking = self.asking.saturating_sub(1);
        // libpulse can answer after close() let go of the stream.
        if self.closed {
            return;
        }
        let info = (self.library.stream_get_timing_info)(self.stream);
        let (Some(info), Some((_, now))) = (info.as_ref(), self.now()) else {
            return;
        };
        if success && info.read_index_corrupt == 0 && info.read_index > self.position {
            self.position = info.read_index;
        }
        let mut realtime = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut realtime);
        // How long ago, by the system's wall clock, the server answered.
        let age = (realtime.tv_sec - info.timestamp.tv_sec) * 1_000_000_000 + realtime.tv_nsec
            - info.timestamp.tv_usec * 1000;
        let answered = now.nseconds() as i64 - age;
        if success
            && info.write_index_corrupt == 0
            && info.playing != 0
            && info.write_index > 0
            && self.stamps.timing(
                info.write_index,
                info.source_usec as i64 * 1000,
                answered,
                now.nseconds() as i64,
            )
        {
            self.resumed = false;
        }
        if !self.holding() {
            self.release();
        }
    }

    unsafe fn read(&mut self) {
        while !self.closed {
            let mut data = std::ptr::null();
            let mut length = 0;
            if (self.library.stream_peek)(self.stream, &mut data, &mut length) < 0 || length == 0 {
                return;
            }
            let start = self.position;
            self.position += length as i64;
            // Without running time the source stopped playing.
            let Some((_, now)) = self.now() else {
                (self.library.stream_drop)(self.stream);
                self.close();
                return;
            };
            // A hole is audio the server skipped.
            if data.is_null() {
                (self.library.stream_drop)(self.stream);
                continue;
            }
            let mut buffer = gst::Buffer::with_size(length).unwrap();
            buffer
                .get_mut()
                .unwrap()
                .copy_from_slice(0, std::slice::from_raw_parts(data.cast::<u8>(), length))
                .unwrap();
            (self.library.stream_drop)(self.stream);
            self.resumed |= self
                .last_arrival
                .is_some_and(|last| now.saturating_sub(last) > FRAGMENT * 4);
            self.last_arrival = Some(now);
            // While holding, each answer about newer audio counts.
            if self.holding()
                || self
                    .last_request
                    .map_or(true, |last| now.saturating_sub(last) >= TIMING_INTERVAL)
            {
                self.request_timing();
            }
            if self.holding() {
                self.held.push((start, buffer));
                // Before the server first answers, as long as video waits
                // for a track, and afterwards a buffer's worth.
                let limit = if self.stamps.offset.is_none() {
                    FIRST_ACCOUNT
                } else {
                    self.buffer
                };
                let held: usize = self.held.iter().map(|(_, buffer)| buffer.size()).sum();
                if held < bytes_of(limit) as usize {
                    continue;
                }
                // A server that doesn't answer: place audio where it
                // arrived, or as it was before.
                self.stamps
                    .offset
                    .get_or_insert(now.nseconds() as i64 - time_of(self.position));
                self.resumed = false;
                if !self.release() {
                    return;
                }
                continue;
            }
            if !self.push(start, buffer) {
                return;
            }
        }
    }

    fn holding(&self) -> bool {
        self.stamps.offset.is_none() || self.resumed || self.stamps.rising.is_some()
    }

    unsafe fn release(&mut self) -> bool {
        std::mem::take(&mut self.held)
            .into_iter()
            .all(|(position, buffer)| self.push(position, buffer))
    }

    // Stamps and pushes audio that starts at `position`. Audio captured
    // before the pipeline played is not part of the recording. False once
    // the source takes no more, after which the stream is closed.
    unsafe fn push(&mut self, mut position: i64, mut buffer: gst::Buffer) -> bool {
        let mut pts = self.stamps.pts(position).unwrap();
        if pts < 0 {
            let early = ((-pts) as u128 * RATE as u128).div_ceil(1_000_000_000) as usize * FRAME;
            if early >= buffer.size() {
                return true;
            }
            buffer = buffer
                .copy_region(gst::BufferCopyFlags::MEMORY, early..)
                .unwrap();
            position += early as i64;
            pts = self.stamps.pts(position).unwrap().max(0);
        }
        let duration = time_of(position + buffer.size() as i64) - time_of(position);
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(gst::ClockTime::from_nseconds(pts as u64));
            buffer.set_duration(gst::ClockTime::from_nseconds(duration as u64));
            if self.end != Some(pts) {
                buffer.set_flags(gst::BufferFlags::DISCONT);
            }
        }
        self.end = Some(pts + duration);
        // Blocks while appsrc is full, so libpulse stops reading and the
        // server holds the rest, and returns when the source stops.
        let pushed = self
            .source
            .upgrade()
            .map(|source| source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]));
        if pushed != Some(gst::FlowReturn::Ok) {
            self.close();
            return false;
        }
        true
    }
}

// How often the server is asked when it captured its audio.
const TIMING_INTERVAL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

// The server's account of when it captured the audio at its write index is
// a few milliseconds late at most: PulseAudio measures its latency on
// request, and pipewire-pulse as of its last graph cycle. Audio the server
// lost, whether its device or graph lost it or PulseAudio dropped it for a
// reader that fell a buffer behind, never reaches its write index, so the
// audio after it was captured later than the audio before it by the amount
// lost. A change of more than STEP that CONFIRM replies with newer audio
// agree on is that, and audio past where the server was capturing when it
// first said so moves later by the least of them. count_lost_audio sees the
// gap. Smaller
// changes, such as one clock running faster than the other, move audio at
// most SLEW per second of running time, which nothing downstream sees as a
// gap. Neither the time audio takes to arrive nor a recorder that stopped
// reading changes the server's account.
const STEP: i64 = crate::pipeline::AUDIO_HOLE.nseconds() as i64;
const CONFIRM: u32 = 3;
const SLEW: i64 = 1_000; // ppm
const WINDOW: usize = 10;

// Where the stream's audio goes in running time: its sample count plus an
// offset from the server's account of when it captured it.
#[derive(Default)]
struct Stamps {
    offset: Option<i64>,
    // Steps not yet applied: the position each applies from and its amount.
    steps: Vec<(i64, i64)>,
    // While the server's account is more than STEP later than the offset:
    // where it was capturing and how much later, in each answer since.
    rising: Option<Vec<(i64, i64)>>,
    // The server's account in recent replies, newest last.
    recent: std::collections::VecDeque<i64>,
    last_write: i64,
    last_reply: i64,
}

impl Stamps {
    fn pts(&mut self, position: i64) -> Option<i64> {
        let offset = self.offset.as_mut()?;
        self.steps.retain(|&(from, amount)| {
            if position >= from {
                *offset += amount;
            }
            position < from
        });
        Some(time_of(position) + *offset)
    }

    // A reply: the server's write index, how long ago it captured the audio
    // there, when it answered, and now, in running time. Returns whether it
    // had newer audio than the last one with any.
    fn timing(&mut self, write: i64, latency: i64, answered: i64, now: i64) -> bool {
        if write <= self.last_write {
            return false;
        }
        self.last_write = write;
        let account = answered - latency - time_of(write);
        // When it answered, the server had captured audio up to here, past
        // its write index by its latency, so audio it lost since is past
        // that.
        let capturing =
            write + (latency as i128 * RATE as i128 / 1_000_000_000) as i64 * FRAME as i64;
        let elapsed = now - std::mem::replace(&mut self.last_reply, now);
        let Some(offset) = self.offset.as_mut() else {
            // The account is a little late, never early, the first one most
            // often, so audio waits for the least of the first few.
            self.recent.push_back(account);
            if self.recent.len() >= CONFIRM as usize {
                self.offset = self.recent.iter().min().copied();
            }
            return true;
        };
        let target = *offset + self.steps.iter().map(|(_, amount)| amount).sum::<i64>();
        if account - target > STEP {
            let rising = self.rising.get_or_insert_with(Vec::new);
            rising.push((capturing, account - target));
            if rising.len() < CONFIRM as usize {
                return true;
            }
            // An answer whose write index stopped short of its time is too
            // late by that. The others agree on the amount, and the loss is
            // past where the first answer that does too was capturing.
            let least = rising[1..].iter().map(|&(_, late)| late).min().unwrap();
            let at = rising
                .iter()
                .find(|&&(_, late)| late <= least + STEP)
                .unwrap()
                .0;
            self.steps.push((at, least));
            self.rising = None;
            self.recent.clear();
            return true;
        }
        self.rising = None;
        self.recent.push_back(account);
        if self.recent.len() > WINDOW {
            self.recent.pop_front();
        }
        let floor = *self.recent.iter().min().unwrap();
        let limit = elapsed.max(0) * SLEW / 1_000_000;
        *offset += (floor - target).clamp(-limit, limit);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    fn bytes(time: i64) -> i64 {
        (time as i128 * RATE as i128 / 1_000_000_000) as i64 * FRAME as i64
    }

    // A server answering every 100 ms of running time for an hour. At
    // running time t, `server(t)` is how much audio it has captured and how
    // late its account of the capture time is. Returns the stamps and, for
    // each answer, the running time, the write index, and the offset in use
    // for audio at the write index, once there is one.
    fn answer(server: impl Fn(i64) -> (i64, i64)) -> (Stamps, Vec<(i64, i64, i64)>) {
        let mut stamps = Stamps::default();
        let mut answers = Vec::new();
        for t in (100 * MS..3_600_000 * MS).step_by(100 * MS as usize) {
            let (captured, late) = server(t);
            let write = bytes(captured);
            stamps.timing(write, 0, t + late, t);
            if let Some(pts) = stamps.pts(write) {
                answers.push((t, write, pts - time_of(write)));
            }
        }
        (stamps, answers)
    }

    // Up to 11 ms late, as PulseAudio's account was at most.
    fn noise(t: i64) -> i64 {
        (t / (100 * MS)).wrapping_mul(2_654_435_761) % (11 * MS)
    }

    #[test]
    fn a_late_account_and_late_answers_move_nothing() {
        let (stamps, answers) = answer(|t| (t, noise(t)));
        assert!(stamps.steps.is_empty());
        for (_, _, offset) in answers {
            assert!((0..=11 * MS).contains(&offset), "{offset}");
        }
        // How long answers take to arrive changes nothing either.
        let mut stamps = Stamps::default();
        for t in (100 * MS..60_000 * MS).step_by(100 * MS as usize) {
            let arrived = t + if (20_000 * MS..40_000 * MS).contains(&t) {
                1500 * MS
            } else {
                0
            };
            stamps.timing(bytes(t), 0, t, arrived);
        }
        assert!(stamps.steps.is_empty() && stamps.offset == Some(0));
    }

    // From 20.05 s, the server's device captured nothing for 300 ms, while
    // the server kept answering. Audio up to then keeps its place, and audio
    // from there on moves 300 ms later.
    #[test]
    fn audio_the_server_lost_moves_later_from_where_it_was_lost() {
        let lost = |t: i64| (t - 20_050 * MS).clamp(0, 300 * MS);
        let mut stamps = Stamps::default();
        // Answers with no newer audio than the last say nothing, so the
        // loss is confirmed at 20.5 s.
        for t in (100 * MS..=20_500 * MS).step_by(100 * MS as usize) {
            stamps.timing(bytes(t - lost(t)), 0, t + noise(t), t);
        }
        let first = bytes(20_050 * MS);
        let mut offset = |position| stamps.pts(position).unwrap() - time_of(position);
        assert!(offset(first - FRAME as i64) < 12 * MS);
        let moved = offset(first);
        assert!((295 * MS..312 * MS).contains(&moved), "{moved}");
        for t in (20_600 * MS..60_000 * MS).step_by(100 * MS as usize) {
            stamps.timing(bytes(t - lost(t)), 0, t + noise(t), t);
            let position = bytes(t - lost(t));
            let offset = stamps.pts(position).unwrap() - time_of(position);
            assert!((295 * MS..312 * MS).contains(&offset), "{offset} at {t}");
        }
        assert!(stamps.steps.is_empty());
    }

    // The server stops from 20 s to 20.8 s, and its device holds 0.32 s and
    // loses the rest. On resuming, the server answers before taking in what
    // the device held, so its write index is still at 20 s, and says that
    // audio there was captured 0.32 s ago. The loss is past the audio held.
    #[test]
    fn audio_lost_after_what_the_device_held_moves_from_there() {
        let mut stamps = Stamps::default();
        for t in (100 * MS..=20_000 * MS).step_by(100 * MS as usize) {
            stamps.timing(bytes(t), 0, t, t);
        }
        stamps.timing(bytes(20_000 * MS), 0, 20_000 * MS, 20_000 * MS);
        let resumed = 20_800 * MS;
        stamps.timing(
            bytes(20_000 * MS) + FRAME as i64,
            320 * MS,
            resumed,
            resumed,
        );
        for t in [20_810, 20_820, 20_830] {
            // What the device held, then audio captured since resuming.
            let write = bytes(20_320 * MS) + bytes((t - 20_800) * MS);
            stamps.timing(write, 0, t * MS, t * MS);
        }
        let held = bytes(20_320 * MS);
        let mut offset = |position| stamps.pts(position).unwrap() - time_of(position);
        assert_eq!(offset(held - FRAME as i64), 0);
        assert_eq!(offset(held + FRAME as i64), 480 * MS);
    }

    // The first answers are the latest ones, and audio starts at the least of
    // the first three.
    #[test]
    fn late_first_answers_move_nothing() {
        let mut stamps = Stamps::default();
        for (t, late) in [(100, 20), (110, 5), (120, 0), (130, 0)] {
            stamps.timing(bytes(t * MS), 0, (t + late) * MS, t * MS);
        }
        assert_eq!(stamps.pts(0), Some(0));
    }

    #[test]
    fn one_late_answer_moves_nothing() {
        let (stamps, answers) = answer(|t| (t, if t == 30_000 * MS { 700 * MS } else { 0 }));
        assert!(stamps.steps.is_empty());
        assert!(answers.iter().all(|&(_, _, offset)| offset == 0));
    }

    #[test]
    fn another_clock_is_followed_without_a_gap() {
        for ppm in [500, -500] {
            let (stamps, answers) = answer(|t| (t + t / 1_000_000 * ppm, noise(t)));
            assert!(stamps.steps.is_empty(), "{ppm} ppm");
            let mut previous: Option<(i64, i64)> = None;
            for (t, write, offset) in answers {
                let account = t - time_of(write);
                assert!(
                    (offset - account).abs() < 15 * MS,
                    "{ppm} ppm at {t}: {offset} vs {account}"
                );
                if let Some((then, before)) = previous {
                    assert!((offset - before).abs() <= (t - then) * SLEW / 1_000_000 + 1);
                }
                previous = Some((t, offset));
            }
        }
    }

    #[test]
    fn more_audio_than_time_moves_audio_earlier_slowly() {
        let (stamps, answers) = answer(|t| (t + if t > 10_000 * MS { 100 * MS } else { 0 }, 0));
        assert!(stamps.steps.is_empty());
        let at = |seconds: i64| {
            answers
                .iter()
                .find(|answer| answer.0 >= seconds * 1000 * MS)
                .unwrap()
                .2
        };
        assert_eq!(at(10), 0);
        assert!((-51 * MS..=-49 * MS).contains(&at(60)), "{}", at(60));
        assert_eq!(at(200), -100 * MS);
    }
}
