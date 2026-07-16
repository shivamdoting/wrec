# wrec implementation notes

## Current v0 backend

Cargo compiles the Swift capture engine from
`crates/macos/native/capture_engine.swift` into the build output. The daemon
starts that compiled capture engine at runtime. Packaged app builds copy
`daemon` and `capture-engine` into `Wrec*.app/Contents/MacOS`; standalone CLI
packages copy `wrec`, `daemon`, and `capture-engine` into the CLI runtime.

Why this route for v0:

- Uses real native macOS ScreenCaptureKit immediately.
- Keeps the frame path inside Apple's native stack.
- Rust does not receive, copy, or retain raw pixels.
- Uses `SCStreamOutput` and `AVAssetWriter` with HEVC/AAC `.mov` output.

Current recording path:

```text
Rust GPUI app / CLI / agents
  -> control protocol
  -> daemon
  -> spawn compiled Swift capture engine
  -> ScreenCaptureKit SCStream
  -> SCStreamOutput CMSampleBuffer
  -> AVAssetWriter / VideoToolbox
  -> HEVC/AAC .mov
```

The capture engine accepts the selected display/window target, fps, cursor
setting, codec, quality mode, resolution, system audio setting, Wrec-window
hiding, microphone setting, and mic-indicator setting from the daemon. It keeps
ScreenCaptureKit queue depth low and drops samples when the writer is
backpressured rather than allowing memory to grow.

The app and CLI stay above the `control` crate. The daemon is the only process
that owns target listing, permission requests (screen recording and, before a
microphone job starts, microphone access), job queueing, recording state, store
writes, and macOS recorder startup.

The next backend improvement is to keep AVAssetWriter, but reduce avoidable work
around it:

- Enforce preset limits so efficient/balanced recordings cannot accidentally
  run at native 5K or 60 FPS.
- Benchmark CPU, peak RSS, bitrate, and output size across the preset matrix.
- Investigate AVAssetWriter session boundaries for pause/resume. If `endSession`
  at pause and `startSession` at resume produce a gap-free file, we can remove
  the current post-pause per-sample retiming copy.

## Requirements

- Apple Silicon Mac
- macOS 15+
- Full Xcode selected with `xcode-select`
- Screen Recording permission granted for the app/terminal during development

## Run

```bash
cd Developer/ccing/wrec
cargo build -p daemon --bin daemon
cargo run -p app
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

## Package

```bash
./scripts/package-macos.sh
```

By default this creates an ad-hoc signed `dist/dev/Wrec Dev.app` with the
debug Cargo profile. Release packaging is explicit:

```bash
./scripts/package-macos.sh release
```

Release packaging creates `dist/release/Wrec.app` with the release Cargo
profile and a `.dmg`. Builds are ad-hoc signed; we do not notarize. Set
`CODESIGN_IDENTITY` for Developer ID signing and `NOTARIZE=1` with App Store
Connect credentials if that ever changes.

The standalone CLI runtime is packaged separately:

```bash
./scripts/package-cli-macos.sh release
```

That archive contains `wrec`, `daemon`, and `capture-engine` so the CLI can be
installed without installing the app bundle.
