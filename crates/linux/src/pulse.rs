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
type ServerInfoCallback = unsafe extern "C" fn(*mut Context, *const ServerInfo, *mut c_void);

// The start of pa_server_info.
#[repr(C)]
struct ServerInfo {
    user_name: *const c_char,
    host_name: *const c_char,
    server_version: *const c_char,
    server_name: *const c_char,
}
type TimeEvent = c_void;
type TimeCallback =
    unsafe extern "C" fn(*mut c_void, *mut TimeEvent, *const libc::timeval, *mut c_void);

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

            // For tests to stand in for the functions they use. The others
            // abort.
            #[cfg(test)]
            fn none() -> Self {
                Self {
                    $($name: {
                        unsafe extern "C" fn none($(_: $arg),*) $(-> $ret)? {
                            std::process::abort()
                        }
                        none
                    },)*
                }
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
    context_get_server_info: fn(*mut Context, Option<ServerInfoCallback>, *mut c_void) -> *mut Operation;
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
    context_rttime_new: fn(*mut Context, u64, Option<TimeCallback>, *mut c_void) -> *mut TimeEvent;
    context_rttime_restart: fn(*mut Context, *mut TimeEvent, u64);
    rtclock_now: fn() -> u64;
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
            device,
            connected: Some(connected.clone()),
            ..Reader::new(library, source, buffer)
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
    // Since Capture::start uncorked the stream, as the pipeline played.
    started: bool,
    closed: bool,
    // Bytes of the stream so far, received or skipped, so the server's
    // index of the next byte.
    position: i64,
    stamps: Stamps,
    // Audio waiting for the server to say when it captured audio, whether
    // it lost any while audio stopped arriving, or where it lost some.
    held: Vec<(i64, gst::Buffer)>,
    // How many of the last held buffers arrived since the server last
    // answered.
    unanswered: usize,
    // Since audio arrived that could come after a skip, until the server
    // next answers.
    late: bool,
    // Where the last buffer pushed ended.
    end: Option<i64>,
    // Goes off every TIMING_INTERVAL.
    timer: *mut TimeEvent,
    // Questions on the way, and wall_clock's distance when each was asked.
    asking: std::collections::VecDeque<i64>,
    last_arrival: Option<gst::ClockTime>,
    // Since audio stopped arriving for a while, until the server says
    // whether it lost any meanwhile.
    resumed: bool,
    // pipewire-pulse, whose latency is audio in its graph, which it can lose.
    graph_holds: bool,
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
        CONTEXT_READY => {
            // Answered before the stream is, on the same connection.
            let operation = (reader.library.context_get_server_info)(
                context,
                Some(server_info),
                (reader as *mut Reader).cast(),
            );
            if !operation.is_null() {
                (reader.library.operation_unref)(operation);
            }
            reader.open_stream()
        }
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

unsafe extern "C" fn server_info(_: *mut Context, info: *const ServerInfo, reader: *mut c_void) {
    let Some(info) = info.as_ref() else { return };
    if !info.server_name.is_null() {
        let name = CStr::from_ptr(info.server_name).to_string_lossy();
        (*reader.cast::<Reader>()).graph_holds = name.contains("PipeWire");
    }
}

unsafe extern "C" fn stream_read(_: *mut Stream, _: usize, reader: *mut c_void) {
    (*reader.cast::<Reader>()).read();
}

unsafe extern "C" fn timer_went_off(
    _: *mut c_void,
    _: *mut TimeEvent,
    _: *const libc::timeval,
    reader: *mut c_void,
) {
    (*reader.cast::<Reader>()).ask_again();
}

unsafe extern "C" fn timing_updated(_: *mut Stream, success: c_int, reader: *mut c_void) {
    (*reader.cast::<Reader>()).timing_updated(success != 0);
}

// Questions on the way at most, as for a server that stopped answering.
const ASKING: usize = 64;

// A change in wall_clock's distance of more than this is the clock being
// set. Reading the two clocks one after the other takes microseconds.
const CLOCK_SET: i64 = 1_000_000;

// The wall clock, and its distance from the monotonic clock, in
// nanoseconds. NTP adjusts both alike, so the distance changes only when
// someone sets the wall clock, or the machine sleeps, which the monotonic
// clock doesn't count.
fn wall_clock() -> (i64, i64) {
    let read = |clock| {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(clock, &mut time) };
        time.tv_sec * 1_000_000_000 + time.tv_nsec
    };
    let monotonic = read(libc::CLOCK_MONOTONIC);
    let wall = read(libc::CLOCK_REALTIME);
    (wall, wall - monotonic)
}

// How long video waits for an audio track to start.
const FIRST_ACCOUNT: gst::ClockTime = crate::pipeline::HELD_VIDEO_LIMIT;

impl Reader {
    fn new(library: &'static Library, source: &gst::Element, buffer: gst::ClockTime) -> Self {
        Self {
            library,
            context: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            device: None,
            buffer,
            source: source.downgrade(),
            connected: None,
            started: false,
            closed: false,
            position: 0,
            stamps: Stamps::default(),
            held: Vec::new(),
            unanswered: 0,
            late: false,
            end: None,
            timer: std::ptr::null_mut(),
            asking: std::collections::VecDeque::new(),
            last_arrival: None,
            resumed: false,
            graph_holds: false,
        }
    }

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
        self.started = true;
        let operation = (self.library.stream_cork)(self.stream, 0, None, std::ptr::null_mut());
        if !operation.is_null() {
            (self.library.operation_unref)(operation);
        }
        self.request_timing();
        self.timer = (self.library.context_rttime_new)(
            self.context,
            (self.library.rtclock_now)() + TIMING_INTERVAL.useconds(),
            Some(timer_went_off),
            (self as *mut Self).cast(),
        );
    }

    unsafe fn ask_again(&mut self) {
        if self.closed {
            return;
        }
        self.request_timing();
        (self.library.context_rttime_restart)(
            self.context,
            self.timer,
            (self.library.rtclock_now)() + TIMING_INTERVAL.useconds(),
        );
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
        if self.closed || self.asking.len() >= ASKING {
            return;
        }
        // Before libpulse reads the wall clock for the question.
        let asked = wall_clock().1;
        let operation = (self.library.stream_update_timing_info)(
            self.stream,
            Some(timing_updated),
            (self as *mut Self).cast(),
        );
        if operation.is_null() {
            return;
        }
        (self.library.operation_unref)(operation);
        self.asking.push_back(asked);
    }

    // The server's write index, when it captured the audio there, and its
    // read index. A read index past the audio received is audio the server
    // skipped, which pipewire-pulse does for a reader that fell a buffer
    // behind, and only says so here. The reply comes after the audio sent
    // before it, and libpulse already took off what it holds, which read()
    // takes all of. So the skip is somewhere in the audio that arrived since
    // the last answer, with no mark where: that audio has no known place, and
    // is lost with what was skipped. Audio that arrived earlier came before
    // it, and read() holds all audio that can come after one.
    unsafe fn timing_updated(&mut self, success: bool) {
        let asked = self.asking.pop_front();
        // libpulse can answer after close() let go of the stream.
        if self.closed {
            return;
        }
        let info = (self.library.stream_get_timing_info)(self.stream);
        let (Some(info), Some((_, now))) = (info.as_ref(), self.now()) else {
            return;
        };
        if success && info.read_index_corrupt == 0 {
            if info.read_index > self.position {
                self.held.truncate(self.held.len() - self.unanswered);
                self.position = info.read_index;
            }
            self.unanswered = 0;
            self.late = false;
        }
        // The server says when it answered by its wall clock. libpulse
        // takes that if it falls between when it asked and heard back by
        // its own, and otherwise halfway between those. A server on this
        // machine reads the same system clock, so either is right unless
        // someone set that clock since the question, which makes the answer
        // seem older or newer by as much. Such an answer says nothing about
        // when.
        let (wall, offset) = wall_clock();
        let set = asked.map_or(true, |asked| (offset - asked).abs() > CLOCK_SET);
        let age = wall - info.timestamp.tv_sec * 1_000_000_000 - info.timestamp.tv_usec * 1000;
        let answered = now.nseconds() as i64 - age;
        let (steps, steady) = (self.stamps.steps.len(), self.stamps.steady);
        if success
            && !set
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
        if self.graph_holds && self.stamps.steps.len() > steps {
            self.drop_held_between(steady, self.stamps.steps.last().unwrap().0);
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
            // pipewire-pulse can send audio to a stream that starts corked,
            // before the pipeline plays. It was captured before the recording
            // began.
            if !self.started {
                (self.library.stream_drop)(self.stream);
                continue;
            }
            // Once started, without running time the source is gone.
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
            // pipewire-pulse skips audio once it holds more than the buffer,
            // all but its newest fragment, and says so only in its next
            // answer. Until then audio after a skip seems captured the skip
            // earlier than it was, more than a buffer before it arrived.
            // Audio that seems over half a buffer late, which leaves room for
            // a late account, waits for that answer.
            self.late |= self.stamps.time(start).is_some_and(|time| {
                now.nseconds() as i64 - time > self.buffer.nseconds() as i64 / 2
            });
            // While holding, each answer about newer audio counts.
            if self.holding() {
                self.request_timing();
            }
            if self.holding() {
                self.held.push((start, buffer));
                self.unanswered += 1;
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
                self.late = false;
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

    // Held audio from `from` to `to` has no known place: it goes, and audio
    // before `from` and from `to` on stays, also from a buffer either falls
    // inside.
    fn drop_held_between(&mut self, from: i64, to: i64) {
        let unanswered = self.held.len() - self.unanswered;
        let first = self.held.get(unanswered).map_or(i64::MAX, |held| held.0);
        self.held = std::mem::take(&mut self.held)
            .into_iter()
            .flat_map(|(position, buffer)| {
                let end = position + buffer.size() as i64;
                if end <= from || position >= to {
                    return [Some((position, buffer)), None];
                }
                let part = |start: i64, stop: i64| {
                    (start < stop).then(|| {
                        let range = (start - position) as usize..(stop - position) as usize;
                        let part = buffer.copy_region(gst::BufferCopyFlags::MEMORY, range);
                        (start, part.unwrap())
                    })
                };
                [part(position, from), part(to, end)]
            })
            .flatten()
            .collect();
        self.unanswered = self.held.iter().filter(|held| held.0 >= first).count();
    }

    fn holding(&self) -> bool {
        self.stamps.offset.is_none() || self.resumed || self.late || self.stamps.rising.is_some()
    }

    unsafe fn release(&mut self) -> bool {
        self.unanswered = 0;
        std::mem::take(&mut self.held)
            .into_iter()
            .all(|(position, buffer)| self.push(position, buffer))
    }

    // Stamps and pushes audio that starts at `position`. Audio captured
    // before the pipeline played is not part of the recording. False once
    // the source takes no more, after which the stream is closed.
    unsafe fn push(&mut self, mut position: i64, mut buffer: gst::Buffer) -> bool {
        // Audio from a step on moves, so a buffer one falls inside goes in two.
        if let Some(from) = self.stamps.step_inside(position, buffer.size() as i64) {
            let at = (from - position) as usize;
            let rest = buffer
                .copy_region(gst::BufferCopyFlags::MEMORY, at..)
                .unwrap();
            let first = buffer
                .copy_region(gst::BufferCopyFlags::MEMORY, ..at)
                .unwrap();
            return self.push(position, first) && self.push(from, rest);
        }
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

// How often the server is asked when it captured its audio. By the clock,
// not as audio arrives, so a question waits at a server that stopped.
// PulseAudio answers it as it resumes, before it reads more of its device,
// so the answer says how much the device held then, and audio lost
// meanwhile comes after that.
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
    last_capturing: i64,
    last_reply: i64,
    // The write index in the last reply that said nothing was lost.
    steady: i64,
}

impl Stamps {
    // The first step that applies from inside the `length` bytes at
    // `position`, on a frame.
    fn step_inside(&self, position: i64, length: i64) -> Option<i64> {
        self.steps
            .iter()
            .map(|&(from, _)| from)
            .filter(|&from| from > position && from < position + length)
            .filter(|&from| (from - position) % FRAME as i64 == 0)
            .min()
    }

    // When the audio at `position` was captured, in running time, as pts
    // will stamp it.
    fn time(&self, position: i64) -> Option<i64> {
        let steps: i64 = self
            .steps
            .iter()
            .filter(|&&(from, _)| position >= from)
            .map(|&(_, amount)| amount)
            .sum();
        Some(time_of(position) + self.offset? + steps)
    }

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
    // there, when it answered, and now, in running time. Returns whether the
    // server had captured more than by the last one that did.
    fn timing(&mut self, write: i64, latency: i64, answered: i64, now: i64) -> bool {
        // When it answered, the server had captured audio up to here, past
        // its write index by its latency, so audio it lost since is past
        // that. The latency comes in whole microseconds, less than half a
        // frame, so the nearest frame is the one the server counted. An
        // answer about no more audio than the last says nothing new about
        // when audio was captured, only that it answered later.
        let capturing = write
            + ((latency as i128 * RATE as i128 + 500_000_000) / 1_000_000_000) as i64
                * FRAME as i64;
        if capturing <= self.last_capturing {
            return false;
        }
        self.last_capturing = capturing;
        let account = answered - latency - time_of(write);
        let elapsed = now - std::mem::replace(&mut self.last_reply, now);
        let Some(offset) = self.offset.as_mut() else {
            self.steady = write;
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
        self.steady = write;
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
        stamps.timing(bytes(20_000 * MS), 320 * MS, resumed, resumed);
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

    // As above, but the device held 15361 frames, which the server says as
    // 320020 µs, a little short of them. The loss is past every one.
    #[test]
    fn a_latency_in_whole_microseconds_counts_every_frame_held() {
        let mut stamps = Stamps::default();
        for t in (100 * MS..=20_000 * MS).step_by(100 * MS as usize) {
            stamps.timing(bytes(t), 0, t, t);
        }
        let resumed = 20_800 * MS;
        let held = bytes(20_000 * MS) + 15361 * FRAME as i64;
        stamps.timing(bytes(20_000 * MS), 320_020_000, resumed, resumed);
        for t in [20_810, 20_820, 20_830] {
            stamps.timing(held + bytes((t - 20_800) * MS), 0, t * MS, t * MS);
        }
        let mut offset = |position| stamps.pts(position).unwrap() - time_of(position);
        assert_eq!(offset(held - FRAME as i64), 0);
        assert!(offset(held) > 479 * MS, "{}", offset(held));
    }

    // A step from inside a buffer: Reader::push sends the buffer in two
    // there, so its audio up to the step keeps its place and from it moves.
    #[test]
    fn a_step_inside_a_buffer_is_found_on_its_frame() {
        let frames = |n: i64| n * FRAME as i64;
        let mut stamps = Stamps {
            offset: Some(0),
            steps: vec![(frames(1000), 480 * MS)],
            ..Stamps::default()
        };
        assert_eq!(stamps.step_inside(frames(520), frames(480)), None);
        assert_eq!(
            stamps.step_inside(frames(960), frames(480)),
            Some(frames(1000))
        );
        assert_eq!(stamps.step_inside(frames(1000), frames(480)), None);
        assert_eq!(stamps.pts(frames(960)), Some(time_of(frames(960))));
        assert_eq!(
            stamps.pts(frames(1000)),
            Some(time_of(frames(1000)) + 480 * MS)
        );
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

    // libpulse and the server behind it, for a Reader: audio and answers
    // arrive in the order the server sends them.
    #[derive(Default)]
    struct Socket {
        audio: std::collections::VecDeque<Vec<u8>>,
        info: Option<Box<TimingInfo>>,
        // Questions the reader asked that the server hasn't answered.
        questions: usize,
        disconnected: bool,
    }

    thread_local! {
        static SOCKET: std::cell::RefCell<Socket> = std::cell::RefCell::default();
    }

    unsafe extern "C" fn peek(
        _: *mut Stream,
        data: *mut *const c_void,
        length: *mut usize,
    ) -> c_int {
        SOCKET.with_borrow(|socket| {
            let audio = socket.audio.front();
            *data = audio.map_or(std::ptr::null(), |audio| audio.as_ptr().cast());
            *length = audio.map_or(0, Vec::len);
        });
        0
    }

    unsafe extern "C" fn drop_audio(_: *mut Stream) -> c_int {
        SOCKET.with_borrow_mut(|socket| socket.audio.pop_front());
        0
    }

    unsafe extern "C" fn ask(
        _: *mut Stream,
        _: Option<SuccessCallback>,
        _: *mut c_void,
    ) -> *mut Operation {
        SOCKET.with_borrow_mut(|socket| socket.questions += 1);
        std::ptr::NonNull::dangling().as_ptr()
    }

    unsafe extern "C" fn timing_info(_: *mut Stream) -> *const TimingInfo {
        SOCKET.with_borrow(|socket| {
            socket
                .info
                .as_deref()
                .map_or(std::ptr::null(), |info| info as *const TimingInfo)
        })
    }

    unsafe extern "C" fn unref(_: *mut Operation) {}

    unsafe extern "C" fn restart(_: *mut Context, _: *mut TimeEvent, _: u64) {}

    unsafe extern "C" fn rtclock_now() -> u64 {
        0
    }

    unsafe extern "C" fn cork(
        _: *mut Stream,
        _: c_int,
        _: Option<SuccessCallback>,
        _: *mut c_void,
    ) -> *mut Operation {
        std::ptr::null_mut()
    }

    unsafe extern "C" fn timer(
        _: *mut Context,
        _: u64,
        _: Option<TimeCallback>,
        _: *mut c_void,
    ) -> *mut TimeEvent {
        std::ptr::null_mut()
    }

    unsafe extern "C" fn set_callback(_: *mut Stream, _: Option<StreamCallback>, _: *mut c_void) {}

    unsafe extern "C" fn set_read_callback(
        _: *mut Stream,
        _: Option<ReadCallback>,
        _: *mut c_void,
    ) {
    }

    unsafe extern "C" fn disconnect(_: *mut Stream) -> c_int {
        SOCKET.with_borrow_mut(|socket| socket.disconnected = true);
        0
    }

    unsafe extern "C" fn stream_unref(_: *mut Stream) {}

    // Frames that hold their index, in two channels of 15 bits.
    fn pcm(frames: std::ops::Range<u64>) -> Vec<u8> {
        frames
            .flat_map(|frame| [frame % 32768, frame / 32768])
            .flat_map(|sample| (sample as f32 / 32768.0).to_le_bytes())
            .collect()
    }

    // A buffer a Reader sent, with its first and last frame.
    #[derive(Debug, PartialEq)]
    struct Sent {
        pts: i64,
        duration: i64,
        discont: bool,
        first: u64,
        last: u64,
    }

    // A Reader on a fake libpulse whose server sends fragment n, frames
    // n * 480 on, at (n + 1) * 10 ms of running time, once it captured it,
    // and says it captured audio a millisecond before it did. The running
    // time stands still where the test sets it.
    struct Wire {
        reader: Box<Reader>,
        clock: gst::Clock,
        pipeline: gst::Pipeline,
        sink: gst::Element,
    }

    impl Wire {
        // Playing, with a reader appsrc started.
        fn new() -> Self {
            let mut wire = Self::unplayed();
            wire.pipeline.set_state(gst::State::Playing).unwrap();
            wire.reader.started = true;
            wire
        }

        // Before the pipeline plays, with no clock yet.
        fn unplayed() -> Self {
            gst::init().unwrap();
            SOCKET.set(Socket::default());
            let library = Box::leak(Box::new(Library {
                stream_peek: peek,
                stream_drop: drop_audio,
                stream_update_timing_info: ask,
                stream_get_timing_info: timing_info,
                operation_unref: unref,
                context_rttime_restart: restart,
                rtclock_now,
                stream_cork: cork,
                context_rttime_new: timer,
                stream_set_state_callback: set_callback,
                stream_set_read_callback: set_read_callback,
                stream_disconnect: disconnect,
                stream_unref,
                ..Library::none()
            }));
            let source = gst::ElementFactory::make("appsrc")
                .property(
                    "caps",
                    gst::Caps::builder("audio/x-raw")
                        .field("format", "F32LE")
                        .field("rate", RATE as i32)
                        .field("channels", CHANNELS as i32)
                        .field("layout", "interleaved")
                        .build(),
                )
                .property("format", gst::Format::Time)
                .property("is-live", true)
                .build()
                .unwrap();
            let sink = gst::ElementFactory::make("appsink")
                .property("sync", false)
                .build()
                .unwrap();
            let pipeline = gst::Pipeline::new();
            pipeline.add_many([&source, &sink]).unwrap();
            source.link(&sink).unwrap();
            let clock = glib::Object::new::<gst::SystemClock>().upcast::<gst::Clock>();
            pipeline.use_clock(Some(&clock));
            pipeline.set_start_time(gst::ClockTime::NONE);
            pipeline.set_base_time(gst::ClockTime::ZERO);
            let wire = Self {
                reader: Box::new(Reader::new(library, &source, gst::ClockTime::SECOND)),
                clock,
                pipeline,
                sink,
            };
            wire.at(0);
            wire
        }

        fn at(&self, ms: i64) {
            let time = gst::ClockTime::from_mseconds(ms as u64);
            self.clock
                .set_calibration(self.clock.internal_time(), time, 0, 1);
        }

        fn audio(&mut self, ms: i64, frames: std::ops::Range<u64>) {
            self.audio_frames(ms, frames.collect());
        }

        fn audio_frames(&mut self, ms: i64, frames: Vec<u64>) {
            self.at(ms);
            let bytes = frames
                .into_iter()
                .flat_map(|frame| [frame % 32768, frame / 32768])
                .flat_map(|sample| (sample as f32 / 32768.0).to_le_bytes())
                .collect();
            SOCKET.with_borrow_mut(|socket| socket.audio.push_back(bytes));
            unsafe { self.reader.read() };
        }

        // The server answers every question on the way: it captured `write`
        // frames, and sent or skipped `read`.
        fn answer(&mut self, ms: i64, write: u64, read: u64) {
            self.answer_with(ms, write, read, 0);
        }

        fn answer_with(&mut self, ms: i64, write: u64, read: u64, latency: u64) {
            self.at(ms);
            for _ in 0..SOCKET.with_borrow_mut(|socket| std::mem::take(&mut socket.questions)) {
                let wall = wall_clock().0 + MS;
                let info = TimingInfo {
                    timestamp: libc::timeval {
                        tv_sec: wall / 1_000_000_000,
                        tv_usec: wall % 1_000_000_000 / 1000,
                    },
                    synchronized_clocks: 1,
                    sink_usec: 0,
                    source_usec: latency,
                    transport_usec: 0,
                    playing: 1,
                    write_index_corrupt: 0,
                    write_index: (write * FRAME as u64) as i64,
                    read_index_corrupt: 0,
                    read_index: (read * FRAME as u64) as i64,
                    configured_sink_usec: 0,
                    configured_source_usec: 0,
                    since_underrun: 0,
                };
                SOCKET.with_borrow_mut(|socket| socket.info = Some(Box::new(info)));
                unsafe { self.reader.timing_updated(true) };
            }
        }

        // The reader's timer.
        fn tick(&mut self, ms: i64) {
            self.at(ms);
            unsafe { self.reader.ask_again() };
        }

        // Audio through libpulse's read callback, at whatever running time
        // there is.
        fn arrive(&mut self, frames: std::ops::Range<u64>) {
            let bytes = pcm(frames);
            let length = bytes.len();
            SOCKET.with_borrow_mut(|socket| socket.audio.push_back(bytes));
            let reader = (&mut *self.reader as *mut Reader).cast();
            unsafe { stream_read(self.reader.stream, length, reader) };
        }

        // The pipeline plays, and appsrc's first need-data starts the
        // reader, as Capture::start does.
        fn play(&mut self) {
            self.pipeline.set_state(gst::State::Playing).unwrap();
            unsafe { self.reader.start() };
        }

        // Fragments sent as captured, each answering the questions on the
        // way, with the timer going off every 100 ms.
        fn steady(&mut self, fragments: std::ops::Range<u64>) {
            for n in fragments {
                let ms = (n as i64 + 1) * 10;
                if ms % 100 == 0 {
                    self.tick(ms);
                }
                self.audio(ms, n * 480..(n + 1) * 480);
                self.answer(ms, (n + 1) * 480, (n + 1) * 480);
            }
        }

        fn sent(self) -> Vec<Sent> {
            let source = self.reader.source.upgrade().unwrap();
            let ended = source.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]);
            assert_eq!(ended, gst::FlowReturn::Ok);
            let mut sent = Vec::new();
            while let Some(sample) = self
                .sink
                .emit_by_name::<Option<gst::Sample>>("try-pull-sample", &[&gst::ClockTime::SECOND])
            {
                let buffer = sample.buffer().unwrap();
                let map = buffer.map_readable().unwrap();
                let frame = |i: usize| {
                    let sample = |j: usize| {
                        (f32::from_le_bytes(map[j * 4..j * 4 + 4].try_into().unwrap()) * 32768.0)
                            .round() as u64
                    };
                    sample(2 * i) + sample(2 * i + 1) * 32768
                };
                sent.push(Sent {
                    pts: buffer.pts().unwrap().nseconds() as i64,
                    duration: buffer.duration().unwrap().nseconds() as i64,
                    discont: buffer.flags().contains(gst::BufferFlags::DISCONT),
                    first: frame(0),
                    last: frame(map.len() / FRAME - 1),
                });
            }
            self.pipeline.set_state(gst::State::Null).unwrap();
            sent
        }
    }

    fn frame_time(frame: u64) -> i64 {
        time_of((frame * FRAME as u64) as i64)
    }

    // Every buffer is stamped when its frames were captured, by the server's
    // account, and marked discont where frames are missing. Returns
    // the frames missing between the first and the last buffer, from and to.
    // The account is a millisecond late, less the time between the test
    // reading the wall clock for an answer and the reader reading it again:
    // microseconds, more on a busy machine.
    fn assert_placed(sent: &[Sent]) -> Vec<(u64, u64)> {
        let mut missing = Vec::new();
        for (i, buffer) in sent.iter().enumerate() {
            let late = buffer.pts - frame_time(buffer.first);
            assert!(
                (0..=1_000_000).contains(&late),
                "{buffer:?} is {late} ns late"
            );
            assert_eq!(
                buffer.duration,
                frame_time(buffer.last + 1) - frame_time(buffer.first),
                "{buffer:?}"
            );
            let next = sent[..i]
                .last()
                .map_or(buffer.first, |before| before.last + 1);
            if buffer.first != next {
                assert!(buffer.discont, "{buffer:?}");
                missing.push((next, buffer.first));
            }
        }
        missing
    }

    // pipewire-pulse's main thread stops for 1.05 s while its data thread
    // keeps capturing. Resuming, it skips all but the newest fragment of its
    // 1 s ring and sends that, then answers the questions asked meanwhile.
    // The fragment can't be placed: only the answer after it says there was
    // a skip, not where. Placed after the audio before the skip, it was a
    // second early.
    #[test]
    fn audio_after_a_skip_in_what_arrived_since_the_last_answer_is_lost_with_it() {
        let mut wire = Wire::new();
        wire.steady(0..300);
        for ms in (3100..=4000).step_by(100) {
            wire.tick(ms);
        }
        wire.audio(4050, 404 * 480..405 * 480);
        wire.answer(4050, 405 * 480, 405 * 480);
        wire.steady(405..500);
        let sent = wire.sent();
        assert_eq!(assert_placed(&sent), [(300 * 480, 405 * 480)]);
        assert_eq!(sent.last().unwrap().last, 500 * 480 - 1);
    }

    // The reader stops at 3 s for 2.5 s. Half a second of audio waits in the
    // socket, and the server's next fragment waits for room there, so the
    // server holds the rest and overflows. When the reader takes up again,
    // the server answers after that audio, which keeps its place, then skips
    // and sends its newest fragment, which arrives promptly but is lost with
    // the skip. Placed by when it arrived, the audio waiting in the socket
    // would have looked lost.
    #[test]
    fn audio_held_in_the_socket_keeps_its_place_and_audio_after_a_skip_is_lost() {
        let mut wire = Wire::new();
        wire.steady(0..300);
        for n in 300..351 {
            wire.audio(5500, n * 480..(n + 1) * 480);
        }
        wire.answer(5500, 550 * 480, 351 * 480);
        wire.audio(5505, 549 * 480..550 * 480);
        wire.answer(5505, 550 * 480, 550 * 480);
        wire.steady(550..650);
        let sent = wire.sent();
        assert_eq!(assert_placed(&sent), [(351 * 480, 550 * 480)]);
        assert_eq!(sent.last().unwrap().last, 650 * 480 - 1);
    }

    // The same for 0.6 s, which the server holds: nothing is lost.
    #[test]
    fn audio_held_in_the_socket_and_the_server_is_all_kept() {
        let mut wire = Wire::new();
        wire.steady(0..300);
        for n in 300..351 {
            wire.audio(3600, n * 480..(n + 1) * 480);
        }
        wire.answer(3600, 360 * 480, 351 * 480);
        for n in 351..360 {
            wire.audio(3605, n * 480..(n + 1) * 480);
        }
        wire.steady(360..450);
        let sent = wire.sent();
        assert_eq!(assert_placed(&sent), []);
        assert_eq!(sent.last().unwrap().last, 450 * 480 - 1);
    }

    // pipewire-pulse stops whole for 300 ms at 3 s; its graph keeps going
    // and its stream misses 298 ms of cycles. Resuming, it answers first,
    // with the write index of its last cycle, frame 144000, and 240 frames
    // still in its graph, of which 96 arrive and the rest are lost. Then it
    // sends those 96 frames and audio captured 298 ms later in one fragment.
    // The step goes at the write index plus the latency; the 144 frames
    // before it that came after the loss have no known place and go with it.
    #[test]
    fn audio_pipewire_pulse_took_in_after_a_loss_it_had_not_reported_is_lost_with_it() {
        let sent = graph_loss(144000, &[144000], false);
        assert_eq!(assert_placed(&sent), [(144000, 144240 + LOST)]);
    }

    // Frames pipewire-pulse lost in its graph in graph_loss.
    const LOST: u64 = 298 * 48;

    // pipewire-pulse says it captured audio up to frame 144000 when it has
    // sent audio up to `sent`, then stops as above. Resuming, it sends the
    // audio from `sent` in buffers that start at `cuts`, before or after its
    // second answer.
    fn graph_loss(sent: u64, cuts: &[u64], answer_first: bool) -> Vec<Sent> {
        let mut wire = Wire::new();
        wire.reader.graph_holds = true;
        wire.steady(0..299);
        wire.tick(3000);
        if sent > 299 * 480 {
            wire.audio(3000, 299 * 480..sent);
        }
        wire.answer(3000, 300 * 480, sent);
        for ms in [3100, 3200, 3300] {
            wire.tick(ms);
        }
        wire.answer_with(3300, 300 * 480, sent, 5000);
        if answer_first {
            wire.tick(3313);
            wire.answer_with(3313, 301 * 480, sent, 5000);
        }
        let truth = |frame: u64| if frame < 144096 { frame } else { frame + LOST };
        for (i, &start) in cuts.iter().enumerate() {
            let end = cuts.get(i + 1).copied().unwrap_or(301 * 480);
            wire.audio_frames(3313, (start..end).map(truth).collect());
        }
        if !answer_first {
            wire.answer_with(3313, 301 * 480, 301 * 480, 5000);
        }
        for n in 301..400 {
            let ms = (n as i64 + 1) * 10 + 303;
            wire.tick(ms);
            wire.audio_frames(ms, (n * 480 + LOST..(n + 1) * 480 + LOST).collect());
            wire.answer_with(ms, (n + 1) * 480, (n + 1) * 480, 5000);
        }
        wire.sent()
    }

    // When the server stops before it sends what it said it captured, the
    // audio before that arrives with the loss and keeps its place, whether
    // one buffer holds both ends of the audio that goes, two hold one each,
    // or none holds either, and whether it arrives before or after an answer.
    #[test]
    fn audio_from_before_pipewire_pulse_said_it_captured_it_keeps_its_place() {
        let (prefix, kept) = ((143760, 143999), (144240 + LOST, 144479 + LOST));
        for (sent, cuts, answer_first, around) in [
            (
                143760,
                &[143760][..],
                false,
                [(143520, 143759), prefix, kept],
            ),
            (
                143760,
                &[143760][..],
                true,
                [(143520, 143759), prefix, kept],
            ),
            (
                143760,
                &[143760, 144120][..],
                false,
                [(143520, 143759), prefix, kept],
            ),
            (
                143520,
                &[143520, 144000, 144240][..],
                false,
                [(143040, 143519), (143520, 143999), kept],
            ),
        ] {
            let sent = graph_loss(sent, cuts, answer_first);
            let lost = assert_placed(&sent);
            assert_eq!(lost, [(144000, 144240 + LOST)], "{cuts:?} {answer_first}");
            let at = sent
                .iter()
                .position(|buffer| buffer.first == around[0].0)
                .unwrap();
            let got: Vec<_> = sent[at..at + 3].iter().map(|b| (b.first, b.last)).collect();
            assert_eq!(got, around, "{cuts:?} {answer_first}");
            // The gap count_lost_audio reports, within the account's lateness.
            let gap = sent[at + 2].pts - (sent[at + 1].pts + sent[at + 1].duration);
            let missing = frame_time(144240 + LOST) - frame_time(144000);
            assert!(
                (gap - missing).abs() <= MS,
                "{cuts:?} {answer_first}: {gap} ns, not {missing}"
            );
        }
    }

    // A buffer a step falls inside goes out in two: its audio up to the step
    // where it was, and from the step on moved by it, as a discontinuity.
    #[test]
    fn a_buffer_with_a_step_inside_is_sent_in_two() {
        let mut wire = Wire::new();
        let at = |frame: i64| frame * FRAME as i64;
        wire.reader.stamps = Stamps {
            offset: Some(0),
            steps: vec![(at(1000), 480 * MS)],
            ..Stamps::default()
        };
        for frames in [480..960, 960..1440] {
            let start = at(frames.start as i64);
            let buffer = gst::Buffer::from_mut_slice(pcm(frames));
            assert!(unsafe { wire.reader.push(start, buffer) });
        }
        let time = frame_time;
        assert_eq!(
            wire.sent(),
            [
                Sent {
                    pts: time(480),
                    duration: time(960) - time(480),
                    discont: true,
                    first: 480,
                    last: 959,
                },
                Sent {
                    pts: time(960),
                    duration: time(1000) - time(960),
                    discont: false,
                    first: 960,
                    last: 999,
                },
                Sent {
                    pts: time(1000) + 480 * MS,
                    duration: time(1440) - time(1000),
                    discont: true,
                    first: 1000,
                    last: 1439,
                },
            ]
        );
    }

    // A reader libpulse connected, with a stream that starts corked.
    fn connected() -> Wire {
        let mut wire = Wire::unplayed();
        wire.reader.stream = std::ptr::NonNull::dangling().as_ptr();
        assert!(wire.reader.now().is_none(), "no running time before play");
        wire
    }

    // pipewire-pulse can send a fragment to a stream that starts corked,
    // before the pipeline plays, when there is no running time yet. That
    // audio was captured before the recording began and goes, and the stream
    // stays open: the track starts with the audio after it, where it was
    // captured. A reader that closed the stream on it left the movie without
    // the track.
    #[test]
    fn audio_the_server_sent_before_the_pipeline_played_goes_and_the_track_starts_after_it() {
        let mut wire = connected();
        // Frames 0 to 479, captured in the 10 ms before running time 0.
        wire.arrive(0..480);
        assert!(SOCKET.with_borrow(|socket| socket.audio.is_empty()));
        wire.play();
        for n in 1..300 {
            let ms = n as i64 * 10;
            if ms % 100 == 0 {
                wire.tick(ms);
            }
            wire.audio(ms, n * 480..(n + 1) * 480);
            wire.answer(ms, (n + 1) * 480, (n + 1) * 480);
        }
        let sent = wire.sent();
        assert!(!sent.is_empty(), "the track has no audio");
        assert_eq!(sent[0].first, 480, "{:?}", sent[0]);
        // Frame 480 was captured at running time 0.
        let from_480: Vec<_> = sent
            .iter()
            .map(|buffer| Sent {
                first: buffer.first - 480,
                last: buffer.last - 480,
                ..*buffer
            })
            .collect();
        assert_eq!(assert_placed(&from_480), []);
        assert_eq!(sent.last().unwrap().last, 300 * 480 - 1);
    }

    // Once the reader started, audio without running time means the source
    // is gone, as when the pipeline let go of it: the reader takes the audio
    // and closes the stream at once.
    #[test]
    fn audio_after_the_source_went_closes_the_stream() {
        let mut wire = connected();
        wire.play();
        wire.audio(10, 0..480);
        wire.answer(10, 480, 480);
        wire.pipeline.set_state(gst::State::Null).unwrap();
        let source = wire.reader.source.upgrade().unwrap();
        wire.pipeline.remove(&source).unwrap();
        drop(source);
        assert!(wire.reader.now().is_none());
        wire.arrive(480..960);
        assert!(wire.reader.closed);
        assert!(SOCKET.with_borrow(|socket| socket.disconnected && socket.audio.is_empty()));
    }
}
