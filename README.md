# wrec

An M-series-first macOS screen recorder focused on a low-copy, hardware-accelerated capture pipeline with a GPUI interface.

## Architecture

wrec is a Rust app with a small native macOS capture helper.

- `crates/app` owns the GPUI window, controls, notifications, and app state.
- `crates/core` defines shared recorder types: settings, targets, sessions,
  metrics, and the recorder engine trait.
- `crates/daemon` owns local IPC, one active recording job, queued jobs, and
  shared recording control for the app, CLI, and agents.
- `crates/macos` implements the macOS recorder engine. It supervises the native
  helper process and translates helper output into recorder events.
- `crates/store` writes recording history, events, and metrics to SQLite.
- `crates/macos/native/wrec_helper.swift` is the native capture and encode path.

At runtime the flow is:

```text
User changes settings in the GPUI app
  -> app submits recording control to the local coordinator daemon
  -> daemon starts one active macOS recorder job or queues the request
  -> macOS engine starts the compiled Swift helper
  -> helper captures and writes the .mov file
  -> helper emits progress/errors/metrics
  -> daemon persists records in SQLite and exposes job state over IPC
  -> app/CLI poll job state and update their UI/output
```

The media path stays inside Apple's native stack:

```text
Rust GPUI app / CLI / agents
  -> local coordinator daemon
  -> macOS recorder engine
  -> Swift helper
  -> ScreenCaptureKit SCStream
  -> screen frames and optional system audio sample buffers
  -> AVAssetWriter
  -> VideoToolbox hardware video encode + AAC audio
  -> .mov
```

Rust never receives, copies, or retains raw pixels or audio samples. It controls
settings and process lifecycle; the helper owns capture, timestamps, encoding,
and finalizing the file.

## Backend

`crates/macos/build.rs` compiles `crates/macos/native/wrec_helper.swift` with
`swiftc` into Cargo's build output. At runtime, the Rust backend launches that
compiled helper directly.
Packaged apps bundle the helper next to the main executable at
`Wrec.app/Contents/MacOS/wrec-helper`; Cargo development falls back to the
helper path emitted by the build script.

The helper:

- Lists displays and windows with ScreenCaptureKit.
- Captures screen frames with `SCStreamOutput`.
- Captures system audio with ScreenCaptureKit when enabled.
- Requests NV12 capture buffers where possible.
- Writes real-time video and AAC audio through `AVAssetWriter`.
- Uses HEVC by default, with H.264 available from the UI.
- Excludes Wrec's own process audio from system-audio capture.
- Keeps ScreenCaptureKit queue depth small.
- Drops samples when the writer is backpressured instead of accumulating memory.
- Finalizes the writer deterministically on stop.

Video owns the recording timeline. The helper starts the writer session at the
first complete screen frame. If system audio is enabled, audio buffers are
appended with their original ScreenCaptureKit timestamps after the video session
has started.

## UI

The app uses GPUI and `gpui-component` controls.

Current controls:

- Source: display or window.
- Target picker with refresh.
- FPS: 30 or 60.
- Codec: HEVC or H.264.
- Cursor capture toggle.
- System audio capture toggle.
- Preset: efficient, balanced, high. Efficient caps capture at 720p/30 FPS,
  balanced caps capture at 1080p/30 FPS, and high allows native/60 FPS.
- Resolution defaults to 1080p, FPS defaults to 30, and preset defaults to
  balanced.
- Output folder picker.
- Recording status and basic metrics.

Recording-affecting controls are disabled while recording so the UI cannot diverge from the active capture session.

## Reliability

- Recording events are session-scoped so stale helper events do not mutate the wrong recording state.
- The macOS recorder only stops the helper on drop when it owns an active session.
- The daemon keeps one active recording job and queues additional requests by default.
- Stop has a timeout and kill fallback.
- Screen Recording permission denial maps to a typed recorder error.
- The Swift helper is compiled during Cargo checks/tests, so Swift API breakage is caught early.

## Data

- App config and SQLite data live in `~/Library/Application Support/Wrec`.
- Recordings default to `~/Movies/<app name>`.
- Recording events and metrics are stored separately from the media file so the
  UI can show history and debugging information without inspecting `.mov`
  contents.

## Requirements

- macOS with ScreenCaptureKit support.
- macOS 15+ for the current target.
- Apple Silicon is the primary target.
- Full Xcode selected with `xcode-select`.
- Screen Recording permission granted for the app/terminal during development.
- Audio Recording permission granted when system audio capture is enabled.

## Run

```bash
cargo run -p wrec-app
```

The terminal client is intended to be automation-first. It uses the same saved
settings as the app, with flags acting as per-run overrides:

```bash
cargo run -p wrec -- targets --json
cargo run -p wrec -- record start --target display:1 --duration 30s
cargo run -p wrec -- record start --app Safari --duration 5m --json
cargo run -p wrec -- jobs --json
cargo run -p wrec -- job pause <job-id>
cargo run -p wrec -- job resume <job-id>
cargo run -p wrec -- job stop <job-id>
cargo run -p wrec -- daemon stop
```

`list` remains an alias for `targets`, and `record` remains an alias for
`record start`. The CLI now talks to a local coordinator daemon over a Unix
socket so multiple clients can submit work while Wrec keeps one active recording
at a time. Additional requests queue by default; pass `--no-queue` to fail when
another recording is active, or `--detach` to submit and return immediately.
For foreground `record start` commands, Ctrl+C asks the daemon to stop the
submitted job before the CLI exits.

Daemon runtime files are intentionally agent-accessible:

```text
~/.wrec/wrec.sock
~/.wrec/daemon.log
~/.wrec/job-events.jsonl
```

Set `WREC_HOME` to override that directory for tests or isolated agents.

If GPUI shader compilation fails, select full Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

If `metal` still reports a missing Metal Toolchain, download Apple's Metal component:

```bash
xcodebuild -downloadComponent MetalToolchain
```

## Checks

```bash
cargo fmt
cargo check
cargo test
```

## Package

The macOS packaging script has two channels.

Contributor/dev builds are the default:

```bash
./scripts/package-macos.sh
```

This uses Cargo's dev profile and creates `dist/dev/Wrec Dev.app`.
Dev packaging also writes `dist/dev/README.md` with the local open/rebuild
commands and generated build details.

Release builds are explicit:

```bash
./scripts/package-macos.sh release
```

This uses Cargo's release profile and creates `dist/release/Wrec.app`.
Release packaging does not create a companion README.

Both channels copy the Rust GPUI app as `wrec-app`, copy the terminal client as
`wrec`, copy the compiled Swift `wrec-helper`, and sign each executable.
Dev builds use the bundle identifier `app.wrec.wrec.dev`; release builds use
`app.wrec.wrec`.

After installation, the bundled CLI lives at
`/Applications/Wrec.app/Contents/MacOS/wrec`.

Local packaging uses ad-hoc signing by default. Developer ID signing and
notarization can be enabled for release builds with environment variables:

```bash
CODESIGN_IDENTITY="Developer ID Application: Example, Inc. (TEAMID)" \
APPLE_ID="dev@example.com" \
APPLE_TEAM_ID="TEAMID" \
APPLE_APP_PASSWORD="app-specific-password" \
NOTARIZE=1 \
./scripts/package-macos.sh release
```

Runtime app data lives in `~/Library/Application Support/Wrec`.
Recordings default to `~/Movies/<app name>`.

Pushing a `v*` tag whose commit is on `main` runs the release workflow and
uploads the notarized `.dmg` to GitHub Releases.

## Current Limitations

- Microphone capture is not implemented.
- Output is `.mov`.
- Compression stays AVAssetWriter-managed so disk writing, audio muxing, and
  media finalization remain owned by AVFoundation.
- Pause/resume currently retimes samples after a pause. The next efficiency
  pass is to verify AVAssetWriter session boundaries for pause gaps so we can
  avoid copying every later sample buffer.
- The Swift helper is still out-of-process. Packaged builds bundle and codesign it inside the `.app`; replacing it with an in-process native library remains the cleaner long-term shape.
