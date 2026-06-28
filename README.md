# wrec

<p align="center">
  <img src="images/wrec.png" alt="wrec" width="112" />
</p>

Wrec is a Mac screen recorder built around one goal: record efficiently without
chewing through CPU or memory.

It records displays or windows with a native ScreenCaptureKit pipeline, writes
hardware-encoded `.mov` files, and gives you both a small GPUI app and a
JSON-friendly CLI for scripts and agents.

> Heads up: Wrec is still early public software. The current GitHub release
> artifacts are unsigned dev builds, so macOS will show an unsigned-app warning.

## Why Wrec

Most screen recorders grow around editing, cloud upload, collaboration, or
streaming. Wrec is intentionally more narrow: capture the right thing, encode it
efficiently, expose reliable controls, and avoid moving raw frames through places
that do not need them.

The basic split is simple: Rust coordinates recording state, UI, CLI, IPC, and
storage. Apple's native stack handles frame capture, audio capture, hardware
encode, and media finalization.

```text
Rust app / CLI / agents
  -> local coordinator daemon
  -> Swift capture engine
  -> ScreenCaptureKit
  -> AVAssetWriter / VideoToolbox
  -> .mov
```

Rust never receives, copies, or holds onto raw pixels or audio samples.

## Features

- Native macOS app built with Rust and GPUI.
- Standalone `wrec` CLI for terminals, scripts, and coding agents.
- Display and window capture.
- HEVC by default, with H.264 available.
- 30 FPS and 60 FPS recording.
- Efficient, balanced, and high presets.
- Resolution controls for 720p, 1080p, 2K, 4K, and native capture.
- Cursor capture, system audio capture, and Wrec-window hiding toggles.
- Pause, resume, stop, queued jobs, and recording status.
- JSON output for target discovery, job control, errors, metrics, and logs.
- Local recording history and metrics stored separately from media files.

## Install

Download the latest macOS app from
[GitHub Releases](https://github.com/shivamhwp/wrec/releases).

The standalone CLI can be installed with:

```bash
curl -fsSL https://wrec-beta.vercel.app/install | sh
```

The CLI installer grabs the release archive for your Mac, installs the runtime
under `/usr/local/lib/wrec`, and places a managed wrapper at
`/usr/local/bin/wrec`.

If you are testing a local CLI archive instead:

```bash
curl -fsSL https://wrec-beta.vercel.app/install | WREC_CLI_ARCHIVE=/path/to/wrec-cli-aarch64-apple-darwin-dev.tar.gz sh
```

## Requirements

- macOS 15+.
- Apple Silicon is the primary target.
- Full Xcode selected with `xcode-select`.
- Screen Recording permission for the app or terminal.
- Audio Recording permission when system audio capture is enabled.

If GPUI shader compilation fails during development, select full Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

If `metal` still reports a missing Metal toolchain:

```bash
xcodebuild -downloadComponent MetalToolchain
```

## Run From Source

During Cargo development, the app and CLI can auto-start the daemon. Building it
once first makes startup a little faster:

```bash
cargo build -p daemon --bin daemon
cargo run -p app
```

Run the CLI from source:

```bash
cargo run -p cli -- targets --json
cargo run -p cli -- record start --target display:1 --duration 30s
```

## CLI

The CLI is built for automation. Use `--json` whenever a command supports it,
discover targets before recording, and treat target ids as temporary for the
current target list.

```bash
wrec targets --json
wrec record start --target display:1 --duration 30s --json
wrec record start --app Safari --duration 5m --json
wrec record start --target window:438 --detach --json
wrec jobs --json
wrec job show <job-id> --json
wrec job pause <job-id> --json
wrec job resume <job-id> --json
wrec job stop <job-id> --json
wrec daemon status --json
```

Useful per-run overrides:

```bash
wrec record start \
  --target display:1 \
  --quality high \
  --resolution native \
  --fps 60 \
  --codec hevc \
  --out ~/Movies/Wrec \
  --cursor \
  --system-audio \
  --hide-wrec \
  --json
```

Extra recording requests queue behind the active job by default. Pass
`--no-queue` to fail when another recording is active, or `--detach` to submit
the job and return immediately.

## Presets

| Preset | Resolution cap | FPS cap | Use it for |
| --- | --- | --- | --- |
| Efficient | 720p | 30 FPS | Long captures and low overhead |
| Balanced | 1080p | 30 FPS | Default everyday recording |
| High | Native, 2K, or 4K | 60 FPS | Detail-heavy captures |

## Runtime Paths

App config and SQLite data:

```text
~/Library/Application Support/Wrec
```

Default recording output:

```text
~/Movies/<app name>
```

Daemon files for local automation:

```text
~/.wrec/wrec.sock
~/.wrec/daemon.log
~/.wrec/job-events.jsonl
```

Set `WREC_HOME` to override the daemon directory for tests or isolated agents.

## Architecture

```text
                  +----------------+
                  |     core       |
                  | domain types   |
                  +--------+-------+
                           |
        +------------------+------------------+
        |                                     |
+-------v------+                      +-------v------+
|   wrec-app   |                      |   wrec CLI   |
| GPUI client  |                      | terminal UX  |
+-------+------+                      +------+-------+
        |                                    |
        +---------------+--------------------+
                        |
                  +-----v------+
                  |  control   |
                  | IPC client |
                  | protocol   |
                  +-----+------+
                        |
                  Unix socket
                        |
                  +-----v------+
                  |   daemon   |
                  | queue/jobs |
                  | settings   |
                  | store I/O  |
                  +-----+------+
                        |
                  +-----v------+
                  |   macos    |
                  | recorder   |
                  +-----+------+
                        |
                  +-----v----------+
                  | capture-engine |
                  | SCK + writer   |
                  +-----+----------+
                        |
        ScreenCaptureKit -> AVAssetWriter -> .mov
```

- `crates/app` owns the GPUI window, controls, notifications, and app state.
- `crates/cli` owns the terminal UX and JSON command surface.
- `crates/core` defines shared recorder types, settings, targets, sessions, and
  metrics.
- `crates/control` defines the IPC protocol, daemon client, daemon discovery,
  and daemon startup.
- `crates/daemon` owns local IPC, one active recording job, queued jobs, and
  persisted recording state.
- `crates/macos` supervises the native capture engine and translates capture
  output into recorder events.
- `crates/store` writes recording history, events, and metrics to SQLite.
- `crates/macos/native/capture_engine.swift` owns ScreenCaptureKit capture and
  AVAssetWriter encode/finalization.

The app and CLI stay thin. They do not import the macOS recorder, store, or
capture-engine code directly. The daemon is the source of truth for recording
state.

## Development

Run checks before sending changes:

```bash
cargo fmt
cargo check
cargo test
```

The marketing site and benchmark helpers use Bun:

```bash
cd marketing
bun install
bun run format
bun run check
```

Do not use npm, pnpm, yarn, or npx here.

Local recording-path benchmarks live in `benchmarks/`:

```bash
cd benchmarks
bun run bench -- --duration 8s
open index.html
```

## Packaging

Create a local dev app:

```bash
./scripts/package-macos.sh
```

This creates `dist/dev/Wrec Dev.app`, uses the dev Cargo profile, signs the app
ad-hoc, and writes `dist/dev/README.md` with the local build details.

Create release artifacts:

```bash
./scripts/package-macos.sh release
./scripts/package-cli-macos.sh release
```

The app package contains `wrec-app`, `daemon`, and `capture-engine`. The CLI
package contains `wrec`, `daemon`, and `capture-engine`, so it can run without
copying anything out of the app bundle.

Pushing a `v*` tag whose commit is on `main` runs the release workflow and
uploads the unsigned dev `.dmg` and CLI archive to GitHub Releases.

## Current Limitations

- Microphone capture is not implemented.
- Output is currently `.mov`.
- Release artifacts are unsigned dev builds and are not notarized.
- Compression is AVAssetWriter-managed so disk writing, audio muxing, and media
  finalization stay inside AVFoundation.
- Pause/resume currently retimes samples after a pause. A future efficiency pass
  should verify AVAssetWriter session boundaries so this copy can be removed.
- The Swift capture engine is still out of process. Packaged builds bundle and
  codesign it inside the app; an in-process native library is the cleaner
  long-term shape.

## Contributing

Wrec's north star is recording efficiency: low memory footprint, low CPU usage,
and clear controls for people and agents. Prefer obvious designs, keep the media
path native, and measure changes that could affect capture overhead.

## License

MIT
