<p align="center">
  <img src="images/wrec.png" alt="wrec" width="112" />
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

[![CI powered by Blacksmith](https://img.shields.io/badge/CI_powered_by-Blacksmith-F0FB29?style=flat-square&labelColor=202020)](https://www.blacksmith.sh/)

Wrec records displays or windows with a native ScreenCaptureKit pipeline, writes
hardware-encoded `.mov` files, and gives you both a small GPUI app and a
JSON-friendly CLI for scripts and agents.

> [!NOTE]
> Wrec is still early public software. Release builds are not notarized, so
> macOS blocks the first launch of the app. Clear the quarantine recursively
> (the nested helpers are quarantined separately) and reopen:
> ```bash
> xattr -dr com.apple.quarantine /Applications/Wrec.app
> ```
> The CLI (installer below) is not affected by the warning.

## Features

- Native macOS app built with Rust and GPUI.
- Standalone `wrec` CLI for terminals, scripts, and coding agents.
- Display and window capture.
- HEVC by default, with H.264 available.
- 30 FPS and 60 FPS recording.
- Resolution controls for 720p, 1080p, 2K, 4K, and native capture.
- Cursor capture, system audio capture, microphone capture, and Wrec-window hiding toggles.
- Pause, resume, stop, queued jobs, and recording status.
- JSON output for target discovery, job control, errors, metrics, and logs.
- Local recording history and metrics stored separately from media files.

## Install

**The app** — download the latest macOS build from
<a href="https://github.com/shivamdoting/wrec/releases" target="_blank" rel="noopener noreferrer">GitHub Releases</a>
and drag it into `/Applications`. After that it updates itself in place:
About → Check for updates (the update is digest-verified and relaunches
without the Gatekeeper warning).

**The CLI** — for terminals, scripts, and coding agents:

```bash
curl -fsSL https://wrec.app/install | sh
```

The CLI installer grabs the archive for your Mac, installs the runtime
under `/usr/local/lib/wrec`, and places a managed wrapper at
`/usr/local/bin/wrec`. Update it later with `wrec update` (or check first
with `wrec update --check`).

## Requirements

- macOS 15+.
- Apple Silicon is the primary target.
- Screen Recording permission for the app or terminal.
- Audio Recording permission when system audio capture is enabled.
- Microphone permission when microphone capture is enabled.

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
```

Set `WREC_HOME` to override the daemon directory for tests or isolated agents.

## Contributing

Building from source, development checks, and packaging live in
[CONTRIBUTING.md](CONTRIBUTING.md). Wrec's north star is recording efficiency:
low memory footprint, low CPU usage, and clear controls for people and agents.

## License

MIT
