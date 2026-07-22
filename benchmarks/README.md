# wrec ground-truth performance benchmarks

This directory contains the pre-release performance gate for wrec.

- `results/` — slim run summaries (verdicts, gates, aggregates; ~100 KB each).
  **Committed to git** — this is the published record of every bench. The UI
  lives on the site: wrec.app/benchmarks (`docs/src/pages/benchmarks/`)
  renders these files at build time.
- `runs/` — full raw run documents (~2 MB each: per-frame decode arrays,
  process samples, event logs). Local debugging only, git-ignored.
- `.tmp/`, `node_modules/` — generated, git-ignored.

Every `v*` release requires a bench: the release workflow fails unless
`results/` contains a passing, non-dirty release-suite summary for a commit
whose only difference from the tagged commit is the `benchmarks/` directory,
and it attaches that summary to the GitHub release as
`wrec-bench-<version>.json`.

The suite is intentionally black-box. It drives wrec only through the public CLI,
uses an isolated `WREC_HOME` and `WREC_DATA_DIR` for each tested binary, and
treats the encoded `.mov` as the source of truth.

## Quick Start

From the repo root:

```sh
bun run bench                 # smoke suite; builds target/release/wrec first
bun run bench release         # gated release profiles
```

The candidate binary is built automatically (`cargo build --release -p cli -p
daemon`) unless `--wrec` or `WREC_BIN` points at a specific one.

View the rendered report — the site builds it from `results/`:

```sh
cd docs && bun run dev   # then open localhost:4321/benchmarks
```

The smoke suite runs one balanced HEVC recording against the stimulus window and
does not apply release gates. It is meant for local sanity checks and CI plumbing.

## Release Gate

Before creating a version tag, run the local release check from a clean checkout
on a Mac with Screen Recording permission:

```sh
bun run release:check --against /path/to/previous-release/wrec
```

This builds the candidate from the current checkout, runs the complete interleaved
A/B suite, and validates the resulting summary against `HEAD`. A failed or
inconclusive run exits non-zero. Commit the new file in `results/` before creating
the version tag; the release workflow runs the same verifier and refuses to
publish without it.

For budget-only investigation outside the release process, run:

```sh
bun run bench release
```

Release profiles:

- `efficient-720p30-hevc`
- `balanced-1080p30-hevc`
- `high-native60-hevc`
- `balanced-1080p30-h264`

Each release profile runs one warmup and three measured reps per binary. With
`--against`, measured reps are interleaved candidate/reference as ABABAB within
each profile. The default release duration is `15s`; `--duration` overrides it.

## Native Helpers

The harness compiles two Swift helpers into `.tmp/` with `swiftc` at run time:

- `native/stimulus.swift`: an AppKit floating window titled
  `wrec-bench-stimulus`. It redraws at 60 Hz and includes a large binary marker
  strip encoding the current stimulus frame index.
- `native/decode.swift`: an AVFoundation decoder that walks every video frame in
  the recorded `.mov` and emits JSON with codec, dimensions, duration, PTS, and
  decoded stimulus indices.

The benchmark resolves the stimulus through `wrec list --json` and records it as
`--target window:<id>`. Static desktops are never used because ScreenCaptureKit
only delivers frames when content changes.

## Gates

Release output uses schema `wrec.perf/v1` and writes one JSON file to
`runs/<timestamp>-<shortsha>.json`. Each profile lists every gate with a name,
threshold, measured value, delta where applicable, and status.

Hard gates:

- observed steady-state FPS >= 97% (30 fps) or 95% (60 fps) of min(profile
  target, stimulus achieved rate) — the recorder is not blamed for frames the
  stimulus never displayed, and 60 fps window capture tops out around 95% of
  the display rate even with zero engine-reported drops
- capture completeness: unique captured stimulus indices over the index span
  that reached the screen during the steady window; gated only when the
  capture rate covers the display rate (a 30 fps profile of a 60 Hz stimulus
  skips every other index by design)
- self-reported drop ratio <= 0.5% at 30 fps or <= 1% at 60 fps
- max inter-frame PTS gap <= 500 ms
- PTS monotonicity
- decoded codec and dimensions match the request
- self-reported frames and decoded unique stimulus frames disagree by <= 5%
- start latency <= 1500 ms
- finalize latency <= 3000 ms, or <= 5000 ms for `high`

A/B gates with `--against`:

- average CPU
- p95 CPU
- max RSS
- start latency
- finalize latency

A/B gates fail only when the candidate is worse than reference by more than 15%
and above the metric noise floor. Same-binary rep spread above twice the noise
floor makes the affected gate `inconclusive`.

Environment preamble captures AC power, thermal state, macOS version, chip,
memory, load average, and git commit/dirty state. Release runs on battery, under
thermal limits, or with high load are marked inconclusive rather than trusted.

## Options

```sh
bun run bench --help
```

Useful options:

```sh
bun run bench --duration 5s
bun run bench release --duration 20s
bun run bench release --sample-interval-ms 250
bun run bench release --wrec target/release/wrec --against /tmp/wrec-ref/wrec
```
