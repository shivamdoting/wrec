# Linux CLI and daemon

The Linux backend records a Wayland display or window selected through the
desktop's ScreenCast portal. It requires PipeWire DMA-BUF capture, GStreamer
1.24 or newer, and an Intel or AMD GPU with VA-API encoding support. This is
an initial backend; hardware compatibility and efficiency still need measurements
on real desktops. There is no Linux GUI in this release.

The pipeline is `pipewiresrc → DMA-BUF → vapostproc → VA encoder → qtmux`.
Rust manages sessions and buffer metadata and never maps video pixels. The
source negotiates four to eight buffers, the video queue holds at most two
frames, and the encoder uses no B-frames. This bounds application queues;
the compositor and driver have their own allocations. Colour conversion and
scaling can still allocate GPU surfaces, so this is not a claim of zero total
copies. The backend rejects CPU frame buffers and has no software video fallback.

## Install dependencies and build

Ubuntu 24.04 or newer is a starting point. The desktop must already have a
working PipeWire service and its own `xdg-desktop-portal` backend, such as
`xdg-desktop-portal-gnome` or `xdg-desktop-portal-kde`. Use the backend for your
desktop; do not install an arbitrary mix of portal backends.

Use the current stable Rust toolchain.

```bash
sudo apt install build-essential pkg-config libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev gstreamer1.0-tools \
  gstreamer1.0-pipewire gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav \
  pipewire-pulse vainfo
cargo build --release -p cli -p daemon
```

Install the VA-API driver for your GPU. Distribution codec packaging and GPU
capabilities vary. Inspect `vainfo`, `gst-inspect-1.0 vapostproc`, and
`gst-inspect-1.0 vah264enc` or `vah264lpenc`. HEVC needs `vah265enc` or
`vah265lpenc`. If the `va` plugin lists zero elements, check the driver and
access to `/dev/dri/renderD*` from the same user running wrec.

Build a relocatable CLI archive with:

```bash
./scripts/package-cli-linux.sh
```

Extract the archive into a prefix such as `~/.local`. It contains `bin/wrec`
and `lib/wrec/daemon`. Put the prefix's `bin` directory on PATH. Native
GStreamer and driver libraries remain system dependencies. Run the CLI in
the logged-in desktop session; it starts the daemon automatically and
inherits the session's Wayland, D-Bus, PipeWire, and audio environment.
After changing sessions, stop and restart the daemon.

## Record

```bash
wrec targets --json
wrec record --target display:0 --codec h264 --duration 10s
wrec record --target window:0 --codec h264 --no-system-audio
```

`display:0` and `window:0` open the desktop's source picker. They are not
native display or window IDs. Only source types advertised by the portal
appear in `targets`. The duration starts after the first encoded frame,
so time spent choosing a source does not shorten the recording. The picker
times out after two minutes and `wrec job stop <id>` cancels it.
Stopping before capture begins produces a cancelled job, without a movie.
Once capture starts, `--duration` counts wall time, including pauses; the
movie itself omits paused time.

Permission status is `unknown` outside a recording because portal grants
belong to individual sessions. `permission.request` does not open a second
picker. Every recording asks for a source; restore tokens, app-name selection,
unattended capture, and arbitrary window enumeration are not implemented.

System audio records the default output monitor through `pipewire-pulse`.
`--mic` adds the default microphone as a separate AAC track. Audio encoding
runs on the CPU. Use `--no-system-audio` to omit system audio; microphone capture
remains opt-in. Configure devices in the desktop's sound settings. A Linux
wrec window-hiding filter and custom microphone indicator are not provided;
the desktop controls its own screen-sharing indication.

Pause and resume use the pipeline clock so paused time is omitted. Stop
drains the encoders and finalizes the movie, including when paused. The
writer emits ten-second movie fragments. An interrupted recording may only
be playable through its last completed fragment. A static desktop requests
one keepalive frame per second to keep the movie timeline advancing.

X11, NVIDIA/NVENC, HDR capture, and desktop-specific capture shortcuts are
outside this first backend. A compositor/driver combination that cannot
share importable DMA-BUFs fails with an error. There is no silent fallback.

## Validate

```bash
sudo apt install ffmpeg dbus-daemon
cargo fmt --check
cargo check --workspace --locked
cargo test --workspace --locked
dbus-run-session -- cargo test -p linux portal_roundtrip -- --ignored
```

The movie tests use synthetic video and a software encoder only inside the
test module. They check pause timing, stop while paused, real AAC tracks,
file readability, capture errors, and rejection of CPU buffers. The portal
test uses a mock ScreenCast service on an isolated D-Bus and checks source
options, denial, cancellation, and session cleanup. These tests do not prove
that a desktop exports DMA-BUFs or that a driver can import them.

Before calling a GPU/desktop combination supported, record actual displays
and windows, include a static screen and motion, check cursor on/off, audio
and microphone sync, pause/resume, stop from the desktop sharing control,
and inspect files with `ffprobe`. Measure CPU, RSS, GPU memory, power, and
compositor overhead at 1080p30 and 4K60. Compare against an idle baseline on
the same machine and report the GPU, driver, desktop, and library versions.
