# wrec implementation notes

## Current backend

Cargo compiles the Swift capture engine from
`crates/macos/native/capture_engine.swift` into the build output. The daemon
starts that compiled capture engine at runtime. Packaged app builds copy
`daemon` and `capture-engine` into `Wrec*.app/Contents/MacOS`; standalone CLI
packages copy `wrec`, `daemon`, and `capture-engine` into the CLI runtime.

Why this route:

- Uses real native macOS ScreenCaptureKit immediately.
- Keeps the frame path inside Apple's native stack.
- Rust does not receive, copy, or retain raw pixels.
- Uses `SCStreamOutput` and `AVAssetWriter` with HEVC/AAC `.mov` output.

Current recording path:

```text
SwiftUI menu-bar app / Rust CLI / agents
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

The app and CLI stay above the `control` crate. The daemon owns target listing,
job queueing, recording state, store writes, permission operations, and macOS
recorder startup. The app owns the user-facing permission flow; keeping the
daemon and capture engine in its launch chain makes macOS attribute Screen
Recording permission to the matching Wrec channel rather than an internal
helper.

The capture engine routes normal stops, macOS Stop Sharing, stream failures,
stdin closure, and SIGINT/SIGTERM through one AVAssetWriter finalization path.
It also writes ten-second movie fragments. SIGKILL or machine failure cannot run
finalization, but the `.mov` remains playable through its last committed
fragment.

## Requirements

- Apple Silicon Mac
- macOS 15+
- Full Xcode selected with `xcode-select`
- Screen Recording permission granted for the app/terminal during development

## Run

```bash
cargo dev
cargo run -p cli -- targets --json
```

If Swift or the native capture engine cannot find the Apple toolchain, select
full Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
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
