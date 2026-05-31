# wrec implementation notes

## Current v0 backend

Cargo compiles the tiny Swift helper from `crates/macos/native/wrec_helper.swift`
into the build output, and the app starts that compiled helper at runtime.
Packaged builds copy the helper into `Wrec.app/Contents/MacOS/wrec-helper`, and
the app resolves that packaged helper before falling back to Cargo's build
output.

Why this route for v0:

- Uses real native macOS ScreenCaptureKit immediately.
- Keeps the frame path inside Apple's native stack.
- Rust does not receive, copy, or retain raw pixels.
- Uses `SCStreamOutput` and `AVAssetWriter` with HEVC/AAC `.mov` output.

Current recording path:

```text
Rust GPUI app
  -> spawn compiled Swift helper
  -> ScreenCaptureKit SCStream
  -> SCStreamOutput CMSampleBuffer
  -> AVAssetWriter / VideoToolbox
  -> HEVC/AAC .mov
```

The helper accepts the selected display/window target, fps, cursor setting,
system audio setting, codec, and quality mode from the GPUI app. The helper
keeps ScreenCaptureKit queue depth low and drops samples when the writer is
backpressured rather than allowing memory to grow.

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
profile and a `.dmg`. Set `CODESIGN_IDENTITY` for Developer ID signing and
`NOTARIZE=1` with App Store Connect credentials to submit and staple the `.dmg`.
