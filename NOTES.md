# wrec implementation notes

## Current v0 backend

The app starts a tiny Swift helper from `crates/wrec-macos/native/wrec_helper.swift`.

Why this route for v0:

- Uses real native macOS ScreenCaptureKit immediately.
- Keeps the frame path inside Apple's native stack.
- Rust does not receive, copy, or retain raw pixels.
- Uses `SCRecordingOutput` with HEVC `.mov` output.

Current recording path:

```text
Rust GPUI app
  -> spawn Swift helper
  -> ScreenCaptureKit SCStream
  -> SCRecordingOutput
  -> HEVC .mov
```

This is efficient enough for first testing, but the next backend should replace `SCRecordingOutput` with:

```text
SCStreamOutput
  -> CMSampleBuffer
  -> CVPixelBuffer / IOSurface
  -> VTCompressionSession HEVC
  -> AVAssetWriter
```

That will provide tighter bitrate/codec/timestamp control.

## Requirements

- Apple Silicon Mac
- macOS 15+
- Full Xcode selected with `xcode-select`
- Screen Recording permission granted for the app/terminal during development

## Run

```bash
cd Developer/ccing/wrec
cargo run -p wrec-app
```

If GPUI shader compilation fails, select full Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

If `metal` still reports a missing Metal Toolchain, download Apple's Metal
component:

```bash
xcodebuild -downloadComponent MetalToolchain
```
