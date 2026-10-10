# Linux CLI and daemon

The Linux backend records Wayland desktops through the ScreenCast portal and
PipeWire, or X11 displays and windows through XShm/XImage. It selects an installed
encoder automatically: Intel/AMD VA-API, NVIDIA NVENC, then software encoding.
A working desktop and the required native plugins are needed. There is no Linux
GUI yet. AMD Radeon capture has been tested on GNOME Wayland; Intel and NVIDIA hardware
remain unverified. Software fallback makes recording possible without a supported GPU,
but uses more CPU.
Software 4K HEVC can also need hundreds of MiB while encoding. That memory
belongs to the capture worker described below, and the system reclaims it when
the worker exits at the end of each recording. On glibc builds, the worker also
asks the allocator to return freed heap pages between encoder attempts.

On compatible Wayland/VA-API systems, the preferred pipeline is
`pipewiresrc → DMA-BUF → vapostproc → VA encoder → mp4mux`. Rust manages sessions
and buffer metadata without mapping video pixels. The source negotiates four to
eight buffers and the capture queue holds at most two frames. Conversion and scaling
can allocate GPU surfaces; this does not imply zero total copies.

Linux recordings are `wrec-<id>.mp4` files in ISO MP4 format (file type
`mp42`, also listing `mp41`, `isom` and `iso2`). macOS recordings are still
QuickTime `.mov` files. On Linux, wrec uses `mp4mux` rather than GStreamer's
QuickTime writer, `qtmux`. For widths 641 through 1052 and heights 480 through
576, `qtmux` guesses a 4:3 or 16:9 TV picture and declares a clean aperture for
it. FFmpeg 8.0 crops to that aperture, so an 800x528 window would play as
704x528; FFmpeg 7.0 ignores it. `mp4mux` is the same GStreamer muxer writing the
ISO variant, and it declares the full picture. Tracks, codecs, timing and
fragments are the same with either. The `.mp4` name matches the contents, so
tools that pick a reader or a MIME type from the extension get MP4. QuickTime
and Safari playback of Linux recordings has not been tested.

When audio is requested, encoded video waits until each audio track starts or
ends. A track that never supplies a sample would otherwise make the whole movie
unreadable. If waiting video reaches three seconds or 32 MiB, recording continues
without the tracks that have not started and logs each omitted source. The startup
queue has four seconds and 40 MiB of capacity. Once it drains, it holds at most one
encoded frame. Audio that starts within the limit keeps its capture timestamp.
Each encoded AAC queue holds up to four seconds, about 64 KB at 128 kbps, to absorb
the movie writer's wait for the next video frame on an idle screen across a pause.

When shared GPU buffers cannot be imported, wrec retries with system-memory
capture and hardware encoding. NVIDIA uses CUDA conversion when available,
otherwise CPU conversion before NVENC. Software encoding uses x264/OpenH264 for
H.264 and x265 for HEVC. Retries happen only before the first encoded frame and
preserve the requested codec. Job events identify the attempted and selected
paths. X11 capture uses system memory even when encoding runs on the GPU.

On Wayland, the requested frame rate is a ceiling. wrec offers the portal every
whole-number maximum from the requested rate down to 1 fps, highest first, and
PipeWire picks the highest one the portal supports. A 60 Hz output recorded at
30 fps captures at up to 30 fps, and a 5 Hz output captures at up to 5 fps. A
fractional refresh rate below the request is rounded down. For example, a
59.94 Hz output recorded at 60 fps captures at up to 59 fps. A portal that
advertises a single fixed fractional maximum below the requested rate,
instead of a range, cannot be negotiated.

VA encoders set their keyframe interval to about two seconds at the
negotiated capture rate, capped by the requested rate. A 5 fps capture gets
an interval of 10 frames even when 60 fps was requested. Using the requested
rate alone can stall older Radeon HEVC encoding at low capture rates. Each
new set of encoder input caps updates the interval after the previous frames
have reached the encoder. Fixed frame rates take precedence over a variable
stream's maximum rate. Missing rate information uses the requested ceiling.

The movie writer uses frame timestamps for playback timing. Refresh-rate
changes can also change the encoder's parameter sets. The movie keeps its
initial track description and carries those parameter sets with each keyframe,
using `avc3` for H.264 and `hev1` for HEVC. This avoids changing the sample
description in a fragmented movie, which can make older versions of
GStreamer's movie muxer write unplayable fragments. Playback requires a reader
that supports these sample entries; FFmpeg playback is covered by the recording
tests.
When an encoder omits a frame's duration, wrec supplies one frame at the
current capture rate, falling back to the requested rate when the encoded caps
have no rate. The last frame then has a nonzero duration in the finalized movie.
The two-frame capture queue counts frames entering and leaving it, subtracting
frames still waiting. Queue-overrun notifications can race with the consumer
and do not prove that a frame was discarded. Counting uses constant storage and
does not modify capture buffers. The count is exact after the queue drains;
sampling while frames are moving can differ by one frame in transit.

wrec tries shared GPU buffers only when `vapostproc` lists DMA-BUF caps with
`format=DMA_DRM` and comes from GStreamer VA 1.24.6 or later. `DMA_DRM` caps
carry the pixel format and modifier with an explicit `drm-format`. GStreamer VA
1.22 describes DMA-BUF with a plain format such as `BGRx` and no modifier.
GStreamer VA before 1.24.6 imports a DMA-BUF only when each buffer's memory
covers a whole video plane. 1.24.6 needs one byte and reads strides from video
metadata. Producers decide the sizes that PipeWire reports for a DMA-BUF.
xdg-desktop-portal-wlr reports a memory size of 0, so its buffers fail that
older check, and the frame is lost before encoding starts. With older VA, wrec
skips the shared-buffer attempt and starts with system-memory capture, still
encoding on the GPU. The skipped attempt matters on a static screen.
xdg-desktop-portal-wlr sends a frame only when the screen changes, so an
attempt that receives the initial frame and fails leaves later attempts with
no frame until something on screen changes.

`pipewiresrc` pauses its stream briefly while starting. When that pause
interrupts a capture, xdg-desktop-portal-wlr sends the unfinished buffer marked
corrupted, with timestamp 0. This was seen with shared GPU buffers on
xdg-desktop-portal-wlr 0.8.1 and PipeWire 1.6.2. wrec stops an attempt whose
first frame is marked corrupted before anything is encoded and moves on to
the next available mode.

KWin can stamp the first frame of an idle screen with the time of its last
repaint, seconds before capture started. GStreamer then held each new frame
for that long, so a recording of a still screen froze for seconds once
something moved. wrec gives `pipewiresrc` no clock, so it passes each frame on
as it arrives, and stamps each frame with the compositor's timestamp rather
than its arrival, so the time the compositor takes to deliver a frame does not
delay video against audio. KWin stamps a frame with the screen's previous presentation, which after
an idle screen is seconds before it rendered the picture. A timestamp no later
than the previous frame's says nothing about when its picture was made, so
that frame is stamped with when it arrived.

That stale stamp also moves video on KDE Plasma 6.6 and 6.7: a change that
follows less than about a second of stillness lands early by the length of the
stillness, and the first change after a resume can be lost. KWin fixed this
upstream for Plasma 6.8. Linux support on KDE is experimental, and the only
validated KDE stack is Ubuntu 26.04's KWin 4:6.6.6-0ubuntu0.1 with that fix
backported into its screencast plugin. The backport, its source pins, build
script, opt-in and undo are in
[`packaging/linux/kwin-6.6.6`](https://github.com/shivamdoting/wrec/tree/main/packaging/linux/kwin-6.6.6)
in the wrec repository. The wrec package does not include, install or load it.

A screen-capture stream can share frames as plain pointers into PipeWire's
memory (MemPtr) instead of file descriptors (MemFd or DMA-BUF). PipeWire
unmaps that memory when the producer exits, even while wrec still uses a
frame. So wrec drops the first MemPtr frame without reading it and has
`pipewiresrc` copy every later frame while it holds PipeWire's lock. A MemPtr
screen that stays still from the start records no video until it changes.
KWin, mutter, xdg-desktop-portal-wlr and gamescope share MemFd or DMA-BUF,
which wrec does not copy.

wrec sets the movie's output size when the first source caps arrive. Changing
a capsfilter normally asks every upstream element to renegotiate, and
`pipewiresrc` answers by disconnecting and reconnecting its PipeWire stream,
even with identical caps. xdg-desktop-portal-wlr 0.8.1 with
ext-image-copy-capture then requests a new frame while the first one is still
pending, breaks the Wayland protocol, and exits. wrec stops that request at
the video queue, so the converters still renegotiate their output and the
capture source keeps its stream. Other renegotiation requests pass through.

When the captured output or window changes size, `pipewiresrc` sends the new
caps without asking downstream elements to set up their buffer pools again.
GStreamer VA `vapostproc` then copies system-memory frames into a pool sized
for the old caps and rejects the first resized frame. After a size change, wrec
sends the allocation query that normal GStreamer negotiation would send, before
the next frame, so the converters size their pools for the new frames.

## Capture worker

xdg-desktop-portal-wlr 0.8.1 has an upstream startup race on its
ext-image-copy-capture path. A consumer pause can leave a capture frame
pending; resuming or starting the next consumer can then create a second
frame and make the backend exit with "session already has a frame object".
PipeWire's GStreamer source pauses and resumes during startup, so successful
recordings on this stack do not establish reliable startup. wrec detects the
backend loss and fails the job, but does not repair the portal. Stock builds
remain affected. Separate local patches for portal-wlr 0.8.1 cancel pending
captures when their buffers are returned or removed, leave incomplete image
buffers empty, and restart capture when new buffers have no image on a static
output or a cancelled frame may have consumed its damage. These patches are external to
wrec and require an opt-in build matched to the desktop's libraries.

The older wlr-screencopy path also has a static-start limitation. The portal
shares a screencopy-manager binding across sessions and asks for each new
session's first frame with damage tracking. The first session consumes the
binding's initial damage; later sessions on an unchanged output can wait
without receiving an initial image. This was reproduced with portal-wlr 0.7.0,
Sway 1.7 and wlroots 0.15.1. Separate local patches for portal-wlr 0.7.1 request
a first image without damage tracking, destroy closed streams promptly, and
limit retries when the consumer holds every buffer. Stock builds remain
affected; repainting or restarting the portal is not treated as a repair.
The Linux test report distributes pinned patch series, build instructions,
checksums and stock-versus-patched evidence. The wrec package does not install
or replace the desktop portal.

The daemon does not run native capture code. For each recording it starts a
capture worker, which is the same daemon executable started again with a private
argument and the daemon's environment. The worker opens the X11 display or the
ScreenCast portal session, runs the GStreamer pipeline, and writes the movie.
Target discovery stays in the daemon. The daemon sends the start request, pause,
resume and stop to the worker's stdin as JSON lines. The worker answers on stdout
with recorder events and pause/resume replies. Anything native libraries print
goes to the worker's stderr, which the daemon copies into its log.

Xlib exits the whole process when its X server connection breaks, and a GPU
driver can crash its process. Either now ends only the worker. When the worker
exits before reporting a result, the daemon marks that job failed with the exit
status and the worker's last stderr lines, then starts the next queued job. The
daemon PID stays the same. The daemon keeps a partial movie only if the worker
reported encoded frames and the file is not empty.

A worker can also freeze or hang inside a native call. The daemon gives every
worker 20 seconds to exit after a stop request, after its final event, after its
output closes, or after the worker reports that it is tearing down a failed,
lost or cancelled recording, then kills it with SIGKILL. The worker sends that
report before it stops the native pipeline, so a GPU or plugin cleanup that
hangs cannot hold the job open. Recording, idle and paused time never start the
deadline. Cleaning up a failed encoder attempt uses a separate deadline. The job's final status is
published only after the worker has been reaped, so the next queued job never
starts while the previous worker still runs. A worker killed this way before it
reported a result fails its job, unless it had already reported a finalized
movie; then the job completes and its status says that cleanup did not finish. While a worker is frozen, pause and resume fail
after the usual 5 seconds and the recording keeps its current state.

The worker runs in its own process group, so Ctrl-C in a terminal reaches the
daemon, which then stops the worker gracefully. `kill <daemon pid>` does the
same and waits up to 15 seconds for finalization before the daemon exits. The
kernel kills the worker with SIGKILL as soon as the daemon process exits for any
reason, so a worker never outlives its daemon. If the daemon is killed or crashes
during a recording, the movie is not finalized. At most, it is playable through
the last completed fragment that reached the disk. Fragments still buffered by
the file writer are lost, which during a disk stall can be up to 32 MiB. In one test, killing the daemon 12 seconds into a
static-desktop recording left an empty file.

While recording, resource use is the daemon plus its worker. Measuring the
daemon PID alone misses the capture pipeline. Find the worker with
`pgrep -P <daemon pid> -x wrec-capture`, where the daemon PID comes from
`wrec daemon status --json`, and add its CPU and RSS to the daemon's.

Native cleanup between encoder attempts has its own 20-second kill/reap bound.
Completing that cleanup clears only its attempt deadline; it does not clear a
user-stop or final-exit deadline, or limit the next healthy recording. The bound
covers native destructors and allocator cleanup. Repeating a cleanup-start
message does not extend an already armed deadline.

## Install dependencies and build

Use GStreamer 1.22 or newer and current stable Rust. The shared-buffer
DMA-BUF/VA path needs GStreamer VA 1.24.6 or newer. Ubuntu 24.04 is a starting
point; package names differ across distributions.

```bash
sudo apt install build-essential pkg-config libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev gstreamer1.0-tools \
  gstreamer1.0-pipewire gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
cargo build --release -p cli -p daemon
```

Wayland needs PipeWire and the ScreenCast portal backend for the desktop, such as
`xdg-desktop-portal-gnome` or `xdg-desktop-portal-kde`. Use the backend for your
desktop. X11 needs an accessible X server and the `ximagesrc` plugin.

### Portal frontend hangs on xdg-desktop-portal before 1.22.1

The portal frontend (`xdg-desktop-portal`) before 1.22.1 loads PipeWire's
realtime module each time it opens a PipeWire remote for a screen cast. That
module asks the frontend's own Realtime portal for limits over D-Bus, and the
frontend can deadlock on itself until the D-Bus call times out after 25
seconds. While it waits, every portal call stalls. After you approve a source,
the job fails with "Opening the PipeWire remote timed out" or "ScreenCast
portal did not respond within 5s".

This was reproduced on KDE Plasma 6.6.6 with Ubuntu's
`xdg-desktop-portal 1.21.1+ds-1ubuntu3.1` and PipeWire 1.6.2. A separate
GStreamer client reproduced it with the wrec daemon stopped: five screen casts
worked and the sixth hung. Installing RealtimeKit did not help; a session still
hung on the sixth try. The fault is in the portal frontend, not in wrec or the
desktop backend, so it can affect any desktop that runs an affected frontend.

Upstream commit
[89f2f5e](https://github.com/flatpak/xdg-desktop-portal/commit/89f2f5e3d219bc5fd66a2505ee772b16022e8575)
("pipewire: Disable loading module-rt") fixes it and is in xdg-desktop-portal
1.22.1. Upgrade the frontend through your distribution when it ships 1.22.1 or
a build with that commit. That version was read but not run in wrec's tests.

Until then, you can turn the realtime module off for the frontend only. wrec
never changes the portal setup itself. On the KDE system above, a frontend
started with this setting passed 10 of 10 repeated screen casts and the full
recording checks. Removing it brought the hang back on the second screen cast.
The commands below apply this setting through a systemd drop-in. Copy
PipeWire's client config and set
`module.rt = false` in its `context.properties`:

```bash
mkdir -p ~/.config/pipewire ~/.config/systemd/user/xdg-desktop-portal.service.d
sed 's/^context.properties = {/&\n    module.rt = false/' \
  /usr/share/pipewire/client.conf > ~/.config/pipewire/wrec-portal.conf
printf '[Service]\nEnvironment=PIPEWIRE_CONFIG_NAME=wrec-portal.conf\n' \
  > ~/.config/systemd/user/xdg-desktop-portal.service.d/wrec-portal.conf
systemctl --user daemon-reload
systemctl --user restart xdg-desktop-portal.service
```

Check that `~/.config/pipewire/wrec-portal.conf` has `module.rt = false` right
under `context.properties = {`. Only the frontend service reads this file;
other PipeWire clients keep the stock config. To undo it after upgrading:

```bash
rm ~/.config/systemd/user/xdg-desktop-portal.service.d/wrec-portal.conf \
  ~/.config/pipewire/wrec-portal.conf
systemctl --user daemon-reload
systemctl --user restart xdg-desktop-portal.service
```

Install the driver for your GPU. Check `vainfo` and
`gst-inspect-1.0 vah264enc` / `vah264lpenc` for Intel/AMD, or
`gst-inspect-1.0 nvh264enc` for NVIDIA. HEVC uses the corresponding H.265 encoder.
If hardware elements are absent, inspect `x264enc`, `openh264enc`, or `x265enc`.
Distribution codec packaging and GPU capabilities vary. VA-API also needs access
to `/dev/dri/renderD*`; NVIDIA needs its driver and encode libraries. Missing
hardware plugins do not prevent software recording.

GStreamer VA encoders before 1.24.3 replace each frame's capture timestamp with
its frame number times the nominal frame duration. When the screen delivers
fewer frames than that rate, the movie plays faster than the recorded time.
With these versions, wrec attaches each frame's capture time to the frame as a
`GstReferenceTimestampMeta` and restores it after the encoder, so VA encoding
keeps the original timing. VA 1.24.3 and later keep timestamps themselves and
skip this step. If a frame loses its capture time inside an affected encoder,
the recording fails with an error instead of writing faster video. This is not
a minimum version, and it does not switch to software encoding.

```bash
./scripts/package-cli-linux.sh
```

Extract the archive into a prefix such as `~/.local`. It contains `bin/wrec`
and `lib/wrec/daemon`. Put the prefix's `bin` directory on PATH. Native libraries
remain system dependencies. The archive targets the build machine's architecture
and libc; it is not a universal binary for every Linux distribution.

Run the CLI in the logged-in desktop session. It starts the daemon automatically
and inherits the display, D-Bus, PipeWire, and audio environment. After changing
sessions, stop and restart the daemon. A container without the desktop sockets or
GPU device access cannot record the host desktop merely because the host has a GPU.

## Record

```bash
wrec targets --json
wrec record --target display:0 --codec h264 --duration 10s
wrec record --target window:0 --codec h264 --no-system-audio
```

On Wayland, `display:0` and `window:0` open the desktop's source picker; only
advertised source types appear. On X11, use the window ID returned by `targets`
instead of `window:0`; named windows and X screen roots are enumerated directly.
A multi-monitor X screen is captured as one desktop. The captured X11 window
must stay viewable: closing it, minimizing it, or any other unmap fails the
recording within about half a second, including window managers that unmap
windows on other workspaces. Wayland is preferred when
both session variables exist, because XWayland cannot capture the whole desktop.

The duration starts after the first encoded frame, so time spent choosing a
source does not shorten recording. The portal picker times out after two minutes;
`wrec job stop <id>` cancels it. Stopping before capture begins produces a cancelled
job without a movie. After capture starts, `--duration` counts wall time including
pauses; the movie omits paused time. Pause keeps the approved source streams alive but drops
video and audio before conversion/encoding; timestamps remove the pause on resume.
Rust only changes buffer metadata, preserving GPU memory. This also avoids
PipeWire source renegotiation on resume. What a pause drops is decided by when it
was captured, not when it arrives. Audio captured before a pause stays even
when it arrives during it, as when a stalled movie writer held the audio back,
and nothing captured while paused stays. A Wayland screen frame was captured
somewhere between its compositor timestamp and its arrival, so a frame whose
span touches a pause is dropped, including frames captured in the last few tens
of milliseconds before the pause. A pause that starts or ends inside
an audio buffer cuts it between samples. Failed starts do not report nonexistent files.

Wayland permission status is `unknown` outside a recording because grants belong
to individual sessions. Every recording asks for a source. Restore tokens,
unattended Wayland capture, and Wayland app-name selection are not implemented.

System audio uses the default PulseAudio output monitor, including through
`pipewire-pulse`. `--mic` adds the default microphone as a separate AAC track.
Audio encoding runs on the CPU. Use `--no-system-audio` when no audio service is
available. Configure devices in desktop sound settings. Linux cannot apply the
shared wrec window-hiding or custom microphone-indicator options; job settings
report them disabled with a warning. The desktop controls its sharing indicator.

wrec reads audio from the audio server with libpulse, which PulseAudio and
`pipewire-pulse` both serve, so recording audio needs `libpulse.so.0`; the
daemon loads it only then. Each audio source keeps up to one second of raw
audio, about 384 kB per F32 stereo track, while downstream stops taking it, and
asks its server to hold another second. While confirming server timing, it can
hold another second, or three seconds before the first timing reply. These
queues are bounded and normally stay nearly empty. Capture latency remains
10 ms.

Each sample goes where the audio server says it captured it. wrec asks the
server ten times a second for the capture time of its newest audio and how much
audio it has captured and sent. Audio that takes longer to arrive, or a recorder
that stops reading for a moment, leaves that account unchanged, so the audio
keeps its place and nothing is reported. Audio the server lost leaves a gap of
the same length, and wrec reports it. That covers audio its device or graph
lost, audio PulseAudio dropped because the recorder fell more than both buffers
behind, and audio `pipewire-pulse` skipped for the same reason.
`pipewire-pulse` skips without marking where and says so only in its next
timing reply, so the audio that arrived since its previous reply has no known
place. wrec drops that audio and reports it with the skip as one gap; in the
controlled tests it was one 10 ms fragment. A device clock that runs faster or
slower than the system clock moves the audio at most 1 ms per second and is not
reported.

This has limits. Changes in the server's account of 20 ms or less are never
reported; the audio moves smoothly instead. A device clock more than 1000 parts
per million slow builds up until it reads as lost audio, and one that fast
leaves the audio drifting late. The length of each gap is reported, but where
in the audio it falls is not guaranteed, and the job events give the time wrec
placed it at. wrec places a loss after the newest audio the server had captured
when it first reported it, counting audio its device still held. PulseAudio
reports a stall before it reads what its device held, so when the server counts
that audio, the gap falls where the loss was; in the controlled tests every
buffer stayed within 11 ms of its capture time. A device holding audio the
server does not count can put the gap after the loss, and the audio between
those boundaries can play early by the whole loss. `pipewire-pulse` also
counts audio still in its graph, which a server stall can lose. When the
server identifies itself as PipeWire and capture stops arriving, wrec discards
the held audio whose position is uncertain after a confirmed loss. It reports
that discard with the loss. This prevented early audio in the controlled
0.1 and 0.3 s server-stall tests, at the cost of 5 to 13 ms of additional
audio per loss. It cannot protect audio already sent before the loss was
known, or a graph loss with no break in arriving audio.
In a virtual machine whose host caps its CPU, starting a recording can push the
guest over the cap, and the host then stops the whole guest for tens of
milliseconds at a time. The guest's audio graph misses cycles, and that audio
never reaches wrec, which reports it. On a 2-vCPU KDE VM capped at 150% this
lost 0.03 to 0.07 s of microphone audio in the first second of 7 of 88
recordings.
When PulseAudio drops audio for a recorder that fell more than both buffers
behind, it drops it in pieces over the following second, and in the controlled
tests 1280 to 2720 frames (27 to 57 ms) of that second played early, by 9 ms up
to the whole loss. The account is only as good as the server's latency
reporting. Clock drift is validated only with a synthetic device 800 parts per
million fast or slow, not with physical microphones. These are experimental
support limits; a completed job alone does not establish A/V sync.

The movie writer takes the next sample of every track in time order, so a track
that stops delivering holds up the others. Encoded video and each audio track
can wait about 4 seconds for the rest. Encoded movie data can wait for the disk
in a 32 MiB buffer, about 16 seconds at 16 Mbit/s. These buffers stay nearly
empty unless something stalls. Past those limits the recording keeps going with
gaps: the capture queue drops video frames, and an audio source's server drops
audio that nothing read in time, which is reported as lost. Audio still waiting in
an audio source or its server when the recording stops never reaches the movie.
When a track's audio ends more than 0.1 s of recorded time before the stop, wrec
reports that end as missing audio; on a normal stop it is a few tens of
milliseconds. A stalled video
encoder always costs the frames captured meanwhile, since raw frames are too
large to keep.

If an audio source fails, for example because PipeWire or PulseAudio restarts,
its track ends at that point and video and the other track keep recording. An
error anywhere else in the audio chain, such as the AAC encoder, still fails the
job. A
completed job is not proof of a whole movie. When frames were dropped, audio has
gaps, or a track ended early or never started, the job finishes `completed` with
a `media_lost` warning that states what is missing, and the job events record
where wrec placed each audio gap. `wrec record` prints the warning when it finishes.

On Wayland, a recording watches four things besides its frames: the ScreenCast
session's `Closed` signal, the D-Bus owner of `org.freedesktop.portal.Desktop`,
the PipeWire remote socket the portal returned, and the selected PipeWire node.
Frames alone cannot show that a source ended, because a static desktop or a
paused recording may deliver none for any length of time. When the desktop
closes the session, for example from its sharing indicator, wrec finalizes the
movie as if `wrec job stop` had run. When the portal loses its owner, the
PipeWire socket hangs up, or the node is removed, the job fails within a
fraction of a second, active or paused, with an event that names the service to restart. Any
partial movie stays on disk. wrec checks the socket with `poll` and never reads
it, so PipeWire's own reader sees all of its data.

The node watch opens a second PipeWire remote through the same portal session,
so it sees only the nodes that session may use. It runs GStreamer's PipeWire
device provider, from the `gstreamer1.0-pipewire` package that recording already
needs, and fails the recording when the device for the selected node is removed.
A portal backend such as xdg-desktop-portal-wlr owns that node, so the node
disappears when the backend exits. A compositor exit is detected only when it
causes one of the four signals above, which has not been verified. If the node is not
visible on the second remote, or PipeWire does not list its nodes within 5
seconds, the job logs that stream removal will not be detected and records
without the watch. If a desktop removes the node before its `Closed` signal
arrives, a stop from its sharing indicator fails the job with the stream-ended
message instead of finalizing it. The watch never stops its device provider.
Before PipeWire 0.3.78, which includes Debian 12's 0.3.65, stopping the provider
can corrupt memory, so its connection stays open until the capture worker exits.

Stop drains the encoders and finalizes the movie, including when paused. If
video still has not reached the movie writer 10 seconds after stop while nothing
waits for the disk or for audio, wrec ends the video track at the writer and
finalizes the movie with the video it already has, and the job reports the
shorter video as lost media. This rescues a movie whose encoder hung; an encoder
that is only very slow loses the frames it had not finished. If that takes more
than 5 more seconds, or a disk stalls for more than 10 seconds during
finalization, the job fails as before. The
writer requests ten-second movie fragments; encoder keyframes can close them
earlier. An interrupted recording may only be playable through its last completed
fragment. Audio sources can deliver their
first samples later than video. The writer keeps that offset with a per-track
edit list, written in the first fragment and again at finalization, instead of
moving each track to start at zero. To do this, it keeps every sample table in
memory until the movie is finalized and then writes a complete index at the end
of the file. Measure long recordings using the whole daemon process tree and
include the time needed to finalize the index. PipeWire requests one keepalive frame per second on a static
desktop. HDR is not supported.

## Validate

```bash
sudo apt install ffmpeg dbus-daemon xvfb x11-apps pipewire pulseaudio
cargo fmt --check
cargo check --workspace --locked
cargo test --workspace --locked
dbus-run-session -- cargo test -p linux portal_roundtrip --locked -- --ignored
cargo test -p linux node_watch --locked -- --ignored
cargo test -p linux pulse_server --locked -- --ignored --test-threads=1
cargo build -p cli -p daemon
python3 scripts/test-capture-linux.py target/debug/wrec
```

The full test suite requires `x265enc` from the GStreamer runtime packages
listed above. The HEVC latency regression fails if that encoder is absent,
so a passing suite always includes it. The private PulseAudio regressions stop
only their own server to check held audio, real loss reporting and A/V alignment;
they require `pulseaudio` and run separately with the command above.

The isolated Xvfb test records actual X11 display/window pixels through the CLI
and daemon, checks H.264/HEVC decoding and timestamps, and exercises pause/resume
and stopping while paused. It also kills the X server during a recording and
during a pause, with a second job queued. Both jobs must fail, the daemon PID must
stay the same, the worker must be reaped, and the same daemon must record again
after a new X server starts on that display. A job queued behind a closed
window, behind an unmapped window, and behind a worker frozen with SIGSTOP must
start at once and complete.
The frozen worker must be killed 20 seconds after stop, and killing the daemon
must kill its worker. It expects a machine without a hardware encoder so
software fallback is exercised. CI also tests the extracted package and headless
errors. Native pipeline tests cover AAC tracks, timing, capture errors, and strict
DMA-BUF rejection. An isolated mock portal covers options, session cleanup,
session `Closed`, portal owner loss and recovery, and PipeWire socket hangup
without consuming socket data. The node watch test starts a private PipeWire
daemon in a temporary directory, without the session bus. It checks that an
idle node is not reported, that removing the node is reported, that a missing
node disables the watch instead of failing, and that the watch stops promptly
after the daemon dies. On PipeWire before 0.3.78, running it under
`taskset -c 0` makes a provider teardown race fail reliably if one returns.

Real hardware validation on Corex (Ryzen 9 5900H / Radeon Cezanne, Ubuntu 26.04,
GNOME Wayland, GStreamer 1.28.2) recorded H.264 displays and an animated HEVC
window through DMA-BUF and VA-API with no fallback. Video/audio decode and
strictly increasing timestamps passed, including live system audio, pause/resume,
and stopping while paused. The 1080p animated-window sample used about 4% of one
CPU core and 129 MiB peak daemon RSS. It recorded about 31 fps with a requested
60 fps ceiling; this is not sustained-60-fps validation or a general benchmark.
That run predates the capture worker. The pipeline ran inside the daemon then, so
the 129 MiB covered both roles. The worker loads its own copy of the executable
and native libraries, so the sum of daemon and worker RSS is expected to be
higher. Corex has not been measured again since the split.

A release build on x86_64 Ubuntu with GStreamer 1.28.2, recording an isolated
Xvfb with the hardware encoder plugins hidden, measured the split directly. The
idle daemon used 9 to 11 MiB RSS. While recording an 800×600 display, the daemon
stayed near 10 MiB and the worker used 32 MiB with software H.264 and 66 MiB
with software HEVC. Native 4K software recordings peaked near 600 MiB worker RSS
and left the daemon at 11 MiB. These numbers cover software encoding on a
virtual X server, not GPU capture.

Intel/NVIDIA, other compositors, live microphone sync, desktop sharing-control
stops, HDR, sustained 60 fps, 4K, GPU memory and power need separate validation.
For each configuration, compare CPU, RSS, GPU memory, power and compositor cost
against an idle baseline, and report GPU, driver, desktop, encoder and library
versions. Software/Xvfb tests alone cannot prove DMA-BUF import or GPU efficiency.
