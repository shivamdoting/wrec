<p align="center">
  <img src="images/wrec-icon.png" alt="wrec" width="112" />
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/wrec-title-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="images/wrec-title-light.svg">
    <img src="images/wrec-title-light.svg" alt="wrec" width="92" />
  </picture>
</p>

<p align="center">
  the most efficient screen recorder for mac.
</p>

<p align="center">
  <a href="https://github.com/shivamdoting/wrec/releases" target="_blank" rel="noopener noreferrer">Download</a>
  &nbsp;·&nbsp;
  <a href="https://wrec.app/docs" target="_blank" rel="noopener noreferrer">Docs</a>
  &nbsp;·&nbsp;
  <a href="https://wrec.app/docs/agents" target="_blank" rel="noopener noreferrer">For your agents</a>
  &nbsp;·&nbsp;
  <a href="CONTRIBUTING.md">Contributing</a>
  &nbsp;·&nbsp;
  <a href="https://wrec.app/docs/agents" target="_blank" rel="noopener noreferrer">CLI</a>
</p>

[![CI powered by Blacksmith](images/blacksmith-ci-badge.svg)](https://www.blacksmith.sh/)

Wrec records displays or windows with a native ScreenCaptureKit pipeline, writes
hardware-encoded `.mov` files, and gives you both a native SwiftUI menu-bar app
and a JSON-friendly CLI for scripts and agents.

> [!NOTE]
> Wrec is still early public software. Release builds are not notarized, so
> macOS blocks the first launch of an app downloaded with a browser. The
> install script below is not affected (and neither is the CLI); if you drag
> the app in from a browser download instead, clear the quarantine
> recursively (the nested helpers are quarantined separately) and reopen:
> ```bash
> xattr -dr com.apple.quarantine /Applications/Wrec.app
> ```

## Features

- Native SwiftUI menu-bar app with no capture code in the UI process.
- Standalone `wrec` CLI for terminals, scripts, and coding agents.
- Display and window capture.
- HEVC by default, with H.264 available.
- 30 FPS and 60 FPS recording.
- Resolution controls for 720p, 1080p, 2K, 4K, and native capture.
- Cursor capture, system audio capture, microphone capture, and Wrec-window hiding toggles.
- Pause, resume, stop, queued jobs, and recording status.
- Recoverable movie output: clean stops finalize exactly, while ten-second
  fragments preserve playable media through the last committed fragment after
  an abrupt interruption.
- JSON output for target discovery, job control, errors, metrics, and logs.
- Local recording history and metrics stored separately from media files.

## Architecture

The app and CLI are thin clients for the same local recording service. The
SwiftUI shell never captures frames itself: both clients send newline-delimited
JSON requests over a channel-specific Unix socket to the Rust daemon. The daemon owns target
discovery, permissions, the recording queue, job state, controls, history, and
metrics.

For an active job, the daemon launches a separate native Swift capture engine.
ScreenCaptureKit delivers frames directly to AVAssetWriter, which performs
hardware encoding and writes the `.mov` file. Rust coordinates the recording
but never copies or processes video frames.

Normal stops, macOS's sharing-control stop, capture errors, and catchable
termination signals all converge on AVAssetWriter finalization. The writer also
commits ten-second movie fragments, so an uncatchable process or machine failure
leaves the file playable through its last completed fragment.

The app bundle contains `wrec-app`, `daemon`, and `capture-engine`. The
standalone CLI package contains `wrec`, `daemon`, and `capture-engine`, so both
interfaces use the same protocol and recording pipeline.

Normal CLI commands launch the matching Wrec app when its daemon is absent.
This keeps macOS Screen Recording permission attached to Wrec, Wrec Nightly,
or Wrec Dev instead of exposing internal helpers in System Settings.

## Install

**The app**:

```bash
curl -fsSL https://wrec.app/install-app | sh
```

The installer downloads the latest release, verifies its checksum, and
installs it into `/Applications` with no Gatekeeper warning (curl downloads
are never quarantined). You can also download the build from
<a href="https://github.com/shivamdoting/wrec/releases" target="_blank" rel="noopener noreferrer">GitHub Releases</a>
and drag it into `/Applications` yourself — see the quarantine note above.
Either way the app then updates itself in place: About → Check for updates
(the update is digest-verified and relaunches without the Gatekeeper
warning).

**The CLI** — for terminals, scripts, and coding agents:

```bash
curl -fsSL https://wrec.app/install | sh
```

The CLI installer grabs the archive for your Mac, installs the runtime
under `/usr/local/lib/wrec`, and places a managed wrapper at
`/usr/local/bin/wrec`. Update it later with `wrec update` (or check first
with `wrec update --check`).

**Nightly** — optimized early-access builds from `main`, isolated from stable:

```bash
curl -fsSL https://wrec.app/install-app | WREC_CHANNEL=nightly sh
curl -fsSL https://wrec.app/install | WREC_CHANNEL=nightly sh
```

Nightly installs as `Wrec Nightly.app` with the `wrec-nightly` CLI.

## Requirements

- macOS 15+.
- Apple Silicon (M-series) Mac. Intel Macs are not supported yet.
- Screen Recording permission for the matching Wrec app.
- Audio Recording permission when system audio capture is enabled.
- Microphone permission when microphone capture is enabled.

## Build channels

| Channel | Audience | App | CLI | Runtime home |
|---|---|---|---|---|
| Dev | Contributors building locally | Wrec Dev | `wrec-dev` | `~/.wrec-dev` |
| Nightly | Users testing work from `main` | Wrec Nightly | `wrec-nightly` | `~/.wrec-nightly` |
| Release | Stable users | Wrec | `wrec` | `~/.wrec` |

Each channel also has its own `~/Library/Application Support/<app name>` data
directory and `~/Movies/<app name>` default recording directory.

Set `WREC_HOME` to override the daemon directory for tests or isolated agents.

## Contributing

Building from source, development checks, and packaging live in
[CONTRIBUTING.md](CONTRIBUTING.md). Wrec's north star is recording efficiency:
low memory footprint, low CPU usage, and clear controls for people and agents.

## License

MIT
