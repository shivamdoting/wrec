# wrec

An M-series-first macOS screen recorder focused on a low-copy, hardware-accelerated capture pipeline with a GPUI interface.

## Current Pipeline

```text
Rust GPUI app
  -> macOS backend process supervisor
  -> compiled Swift capture helper
  -> ScreenCaptureKit SCStream
      -> .screen CMSampleBuffer
          -> CVPixelBuffer / IOSurface, NV12 where possible
          -> AVAssetWriter video input
          -> VideoToolbox hardware HEVC/H.264
      -> .audio CMSampleBuffer when system audio is enabled
          -> AVAssetWriter audio input
          -> AAC
  -> single .mov file
```

The recording path stays inside Apple's native media stack. Rust controls app
state, target selection, settings, process lifecycle, and UI events; it does
not receive, copy, or retain raw pixels or audio samples.

Video owns the recording timeline. The helper starts the writer session at the
first complete screen frame, then appends later system-audio buffers using
their original ScreenCaptureKit timestamps. Any audio buffers that arrive before
the first video frame are dropped instead of buffered, keeping startup memory
bounded.

## Backend

`crates/macos/build.rs` compiles `crates/macos/native/wrec_helper.swift` with `swiftc` into Cargo's build output. At runtime, the Rust backend launches that compiled helper directly.
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

## Workspace

- `crates/app` - GPUI app/window and UI state.
- `crates/core` - shared recorder settings, session, metrics, and engine trait.
- `crates/macos` - macOS backend and compiled native helper.

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

Both channels copy the Rust GPUI app as `wrec`, copy the compiled Swift
`wrec-helper`, and sign both executables. Dev builds use the bundle identifier
`app.wrec.wrec.dev`; release builds use `app.wrec.wrec`.

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

Runtime app data lives in `~/Library/Application Support/<app name>`.
Recordings default to `~/Movies/<app name>`.

Pushing a `v*` tag whose commit is on `main` runs the release workflow and
uploads the notarized `.dmg` to GitHub Releases.

## Current Limitations

- Microphone capture is not implemented.
- Output is `.mov`.
- Compression is currently AVAssetWriter-managed. Moving to an explicit `VTCompressionSession` is the next step if we need lower-level bitrate, keyframe, timestamp, and encoder control.
- The Swift helper is still out-of-process. Packaged builds bundle and codesign it inside the `.app`; replacing it with an in-process native library remains the cleaner long-term shape.
