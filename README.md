# wrec

An M-series-first macOS screen recorder focused on a low-copy, hardware-accelerated capture pipeline with a GPUI interface.

## Current Pipeline

```text
Rust GPUI app
  -> compiled Swift helper
  -> ScreenCaptureKit SCStream
  -> SCStreamOutput CMSampleBuffer
  -> CVPixelBuffer / IOSurface, NV12 where possible
  -> AVAssetWriter / VideoToolbox hardware encoder
  -> .mov
```

The recording path stays inside Apple's native media stack. Rust controls app state, target selection, settings, process lifecycle, and UI events; it does not receive, copy, or retain raw pixels.

## Backend

`crates/macos/build.rs` compiles `crates/macos/native/wrec_helper.swift` with `swiftc` into Cargo's build output. At runtime, the Rust backend launches that compiled helper directly.

The helper:

- Lists displays and windows with ScreenCaptureKit.
- Captures screen frames with `SCStreamOutput`.
- Requests NV12 capture buffers where possible.
- Writes real-time video through `AVAssetWriter`.
- Uses HEVC by default, with H.264 available from the UI.
- Keeps ScreenCaptureKit queue depth small.
- Drops frames when the writer is backpressured instead of accumulating memory.
- Finalizes the writer deterministically on stop.

## UI

The app uses GPUI and `gpui-component` controls.

Current controls:

- Source: display or window.
- Target picker with refresh.
- FPS: 30 or 60.
- Codec: HEVC or H.264.
- Cursor capture toggle.
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

## Current Limitations

- No audio capture yet.
- Output is `.mov`.
- Compression is currently AVAssetWriter-managed. Moving to an explicit `VTCompressionSession` is the next step if we need lower-level bitrate, keyframe, timestamp, and encoder control.
- The compiled helper is a development-time Cargo output. A packaged app should bundle and codesign the helper inside the `.app`, or replace it with an in-process native library.
