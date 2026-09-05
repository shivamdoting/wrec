# Linux CLI and daemon

The Linux backend records Wayland desktops through the ScreenCast portal and
PipeWire, or X11 displays and windows through XShm/XImage. It selects an installed
encoder automatically: Intel/AMD VA-API, NVIDIA NVENC, then software encoding.
A working desktop and the required native plugins are needed. There is no Linux
GUI yet. Hardware compatibility and efficiency still need measurements on real
desktops; software fallback makes recording possible without a supported GPU,
but uses more CPU.

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
A multi-monitor X screen is captured as one desktop. Wayland is preferred when
both session variables exist, because XWayland cannot capture the whole desktop.

The duration starts after the first encoded frame, so time spent choosing a
source does not shorten recording. The portal picker times out after two minutes;
`wrec job stop <id>` cancels it. Stopping before capture begins produces a cancelled
job without a movie. After capture starts, `--duration` counts wall time including
pauses; the movie omits paused time. Failed starts do not report nonexistent files.

Wayland permission status is `unknown` outside a recording because grants belong
to individual sessions. Every recording asks for a source. Restore tokens,
unattended Wayland capture, and Wayland app-name selection are not implemented.

System audio uses the default PulseAudio output monitor, including through
`pipewire-pulse`. `--mic` adds the default microphone as a separate AAC track.
Audio encoding runs on the CPU. Use `--no-system-audio` when no audio service is
available. Configure devices in desktop sound settings. Linux cannot apply the
shared wrec window-hiding or custom microphone-indicator options; job settings
report them disabled with a warning. The desktop controls its sharing indicator.

Stop drains the encoders and finalizes the movie, including when paused. The
writer emits ten-second movie fragments; an interrupted recording may only be
playable through its last completed fragment. PipeWire requests one keepalive
frame per second on a static desktop. HDR is not supported.

## Validate

```bash
sudo apt install ffmpeg dbus-daemon xvfb x11-apps
cargo fmt --check
cargo check --workspace --locked
cargo test --workspace --locked
dbus-run-session -- cargo test -p linux portal_roundtrip --locked -- --ignored
cargo build -p cli -p daemon
python3 scripts/test-capture-linux.py target/debug/wrec
```

The isolated Xvfb test records actual X11 display/window pixels through the CLI
and daemon, checks H.264/HEVC decoding and timestamps, and exercises pause/resume
and stopping while paused. It expects a machine without a hardware encoder so
software fallback is exercised. CI also tests the extracted package and headless
errors. Native pipeline tests cover AAC tracks, timing, capture errors, and strict
DMA-BUF rejection; an isolated mock portal covers options and session cleanup.

These checks do not validate GPU buffer import or a real desktop portal. Before
calling a GPU/desktop combination validated, record displays and windows, static
screens and motion, cursor on/off, audio/microphone sync, and stopping from the
desktop sharing control. Measure CPU, RSS, GPU memory, power, and compositor cost
at 1080p30 and 4K60 against an idle baseline. Report GPU, driver, desktop, encoder,
and library versions. VA-API and NVIDIA performance remain unverified here.
