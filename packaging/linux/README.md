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
`pipewiresrc → DMA-BUF → vapostproc → VA encoder → qtmux`. Rust manages sessions
and buffer metadata without mapping video pixels. The source negotiates four to
eight buffers and the video queue holds at most two frames. Conversion and scaling
can allocate GPU surfaces; this does not imply zero total copies.

When shared GPU buffers cannot be imported, wrec retries with system-memory
capture and hardware encoding. NVIDIA uses CUDA conversion when available,
otherwise CPU conversion before NVENC. Software encoding uses x264/OpenH264 for
H.264 and x265 for HEVC. Retries happen only before the first encoded frame and
preserve the requested codec. Job events identify the attempted and selected
paths. X11 capture uses system memory even when encoding runs on the GPU.

## Capture worker

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
deadline, and neither does tearing down an encoder attempt that will be retried. The job's final status is
published only after the worker has been reaped, so the next queued job never
starts while the previous worker still runs. A worker killed this way before it
reported a result fails its job. While a worker is frozen, pause and resume fail
after the usual 5 seconds and the recording keeps its current state.

The worker runs in its own process group, so Ctrl-C in a terminal reaches the
daemon, which then stops the worker gracefully. `kill <daemon pid>` does the
same and waits up to 15 seconds for finalization before the daemon exits. The
kernel kills the worker with SIGKILL as soon as the daemon process exits for any
reason, so a worker never outlives its daemon. If the daemon is killed or crashes
during a recording, the movie is not finalized. At most, it is playable through
the last completed fragment that reached the disk. Fragments still buffered by
the file writer are lost. In one test, killing the daemon 12 seconds into a
static-desktop recording left an empty file.

While recording, resource use is the daemon plus its worker. Measuring the
daemon PID alone misses the capture pipeline. Find the worker with
`pgrep -P <daemon pid> -x wrec-capture`, where the daemon PID comes from
`wrec daemon status --json`, and add its CPU and RSS to the daemon's.

## Install dependencies and build

Use GStreamer 1.22 or newer and current stable Rust. GStreamer 1.24 or newer is
recommended for the DMA-BUF/VA path. Ubuntu 24.04 is a starting point; package
names differ across distributions.

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

Install the driver for your GPU. Check `vainfo` and
`gst-inspect-1.0 vah264enc` / `vah264lpenc` for Intel/AMD, or
`gst-inspect-1.0 nvh264enc` for NVIDIA. HEVC uses the corresponding H.265 encoder.
If hardware elements are absent, inspect `x264enc`, `openh264enc`, or `x265enc`.
Distribution codec packaging and GPU capabilities vary. VA-API also needs access
to `/dev/dri/renderD*`; NVIDIA needs its driver and encode libraries. Missing
hardware plugins do not prevent software recording.

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
PipeWire source renegotiation on resume. Failed starts do not report nonexistent files.

Wayland permission status is `unknown` outside a recording because grants belong
to individual sessions. Every recording asks for a source. Restore tokens,
unattended Wayland capture, and Wayland app-name selection are not implemented.

System audio uses the default PulseAudio output monitor, including through
`pipewire-pulse`. `--mic` adds the default microphone as a separate AAC track.
Audio encoding runs on the CPU. Use `--no-system-audio` when no audio service is
available. Configure devices in desktop sound settings. Linux cannot apply the
shared wrec window-hiding or custom microphone-indicator options; job settings
report them disabled with a warning. The desktop controls its sharing indicator.

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
message instead of finalizing it.

Stop drains the encoders and finalizes the movie, including when paused. The
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
sudo apt install ffmpeg dbus-daemon xvfb x11-apps pipewire
cargo fmt --check
cargo check --workspace --locked
cargo test --workspace --locked
dbus-run-session -- cargo test -p linux portal_roundtrip --locked -- --ignored
cargo test -p linux node_watch --locked -- --ignored
cargo build -p cli -p daemon
python3 scripts/test-capture-linux.py target/debug/wrec
```

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
after the daemon dies.

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
