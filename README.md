# wrec

An M-series-first macOS screen recorder focused on a low-copy, hardware-accelerated pipeline.

Planned hot path:

```text
ScreenCaptureKit SCStream
  -> CMSampleBuffer
  -> CVPixelBuffer / IOSurface, NV12 where possible
  -> optional Metal compositor
  -> VideoToolbox hardware HEVC encoder
  -> AVAssetWriter .mov
```

UI: GPUI + gpui-component.

Initial target: macOS 15+, Apple Silicon.

Current v0 uses `SCRecordingOutput` as the native recording bridge, so macOS 15 is required. A lower-level `SCStreamOutput -> VideoToolbox -> AVAssetWriter` backend can be added next for more control and macOS 14 support.

## Workspace

- `crates/wrec-app` — small GPUI app/window.
- `crates/wrec-core` — recorder settings, commands, engine trait.
- `crates/wrec-macos` — macOS capture/encoding backend.

## v0 scope

- Full-display capture implemented through native ScreenCaptureKit helper.
- Window capture UI/type scaffolded; native target selection still pending.
- No audio initially.
- HEVC `.mov` output by default.
- Minimal settings UI.
