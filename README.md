# wrec

An M-series-first macOS screen recorder focused on a low-copy, hardware-accelerated capture pipeline with a GPUI interface.

## Architecture

wrec is a Rust app with a small native macOS capture helper.

- `crates/app` owns the GPUI window, controls, notifications, and app state.
- `crates/core` defines shared recorder types: settings, targets, sessions,
  metrics, and the recorder engine trait.
- `crates/macos` implements the macOS recorder engine. It supervises the native
  helper process and translates helper output into app events.
- `crates/store` writes recording history, events, and metrics to SQLite.
- `crates/macos/native/wrec_helper.swift` is the native capture and encode path.

At runtime the flow is:

```text
User changes settings in the GPUI app
  -> app asks the macOS recorder engine to start/stop
  -> macOS engine starts the compiled Swift helper
  -> helper captures and writes the .mov file
  -> helper emits progress/errors/metrics
  -> app updates UI state and persists records in SQLite
```

The media path stays inside Apple's native stack:

```text
Rust GPUI app
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
- Quality: efficient, balanced, high.
- Output folder picker.
- Recording status and basic metrics.

Recording-affecting controls are disabled while recording so the UI cannot diverge from the active capture session.

## Reliability

- Recording events are session-scoped so stale helper events do not mutate the wrong recording state.
- The backend stops the helper on recorder drop.
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
cargo run -p wrec-cli -- targets --json
cargo run -p wrec-cli -- record start --target display:1 --duration 30s
cargo run -p wrec-cli -- record start --app Safari --duration 5m --json
```

`list` remains an alias for `targets`, and `record` remains an alias for
`record start`. Foreground recordings can still be controlled from stdin with
`pause`, `resume`, and `stop`; recordings started with `--duration` keep running
even if stdin is closed.

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

Both channels copy the Rust GPUI app as `wrec`, copy the terminal client as
`wrec-cli`, copy the compiled Swift `wrec-helper`, and sign each executable.
Dev builds use the bundle identifier `app.wrec.wrec.dev`; release builds use
`app.wrec.wrec`.

After installation, the bundled CLI lives at
`/Applications/Wrec.app/Contents/MacOS/wrec-cli`.

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
- Compression is AVAssetWriter-managed, without explicit low-level bitrate, keyframe, or timestamp control.
- The Swift helper is still out-of-process. Packaged builds bundle and codesign it inside the `.app`; replacing it with an in-process native library remains the cleaner long-term shape.
