# wrec implementation notes

## Current v0 backend

Cargo compiles the tiny Swift helper from `crates/macos/native/wrec_helper.swift`
into the build output, and the app starts that compiled helper at runtime.

Why this route for v0:

- Uses real native macOS ScreenCaptureKit immediately.
- Keeps the frame path inside Apple's native stack.
- Rust does not receive, copy, or retain raw pixels.
- Uses `SCStreamOutput` and `AVAssetWriter` with HEVC `.mov` output.

Current recording path:

```text
Rust GPUI app
  -> spawn compiled Swift helper
  -> ScreenCaptureKit SCStream
  -> SCStreamOutput CMSampleBuffer
  -> AVAssetWriter / VideoToolbox
  -> HEVC .mov
```

The helper accepts the selected display/window target, fps, cursor setting,
codec, and quality mode from the GPUI app. The helper keeps ScreenCaptureKit
queue depth low and drops frames when the writer is backpressured rather than
allowing memory to grow.

The next backend improvement is to replace AVAssetWriter-managed compression
with an explicit `VTCompressionSession`:

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
