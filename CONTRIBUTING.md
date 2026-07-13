# Contributing to wrec

Wrec's north star is recording efficiency: low memory footprint, low CPU usage,
and clear controls for people and agents. Prefer obvious designs, keep the media
path native, and measure changes that could affect capture overhead.

## For the love of god, do not use AI to make PRs

Do not point an agent at this repo and submit whatever comes out. AI-generated
PRs waste maintainer time: they look plausible, compile, and are still wrong in
ways that take longer to review than writing the change ourselves. If you did
not write it, run it, and understand every line of it, do not send it.

Using an editor with completions is fine. Submitting a diff you cannot explain
is not. PRs that read like they were generated end-to-end will be closed
without review.

## Getting vouched

This repo uses [vouch](https://github.com/mitchellh/vouch). Pull requests from
authors not listed in [`.github/VOUCHED.td`](.github/VOUCHED.td) are closed
automatically. If you want to contribute, open an issue first, talk through the
change, and a maintainer can vouch for you. Once a maintainer comments
`vouch @your-handle`, reopen or resubmit your PR (or comment `/recheck` on it).

## Requirements

- macOS 15+ on Apple Silicon.
- Full Xcode selected with `xcode-select`.

If GPUI shader compilation fails, select full Xcode:

```bash
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
```

If `metal` still reports a missing Metal toolchain:

```bash
xcodebuild -downloadComponent MetalToolchain
```

## Run From Source

During Cargo development, the app and CLI can auto-start the daemon. `cargo dev`
builds the matching daemon before launching the app:

```bash
cargo dev
```

Run the CLI from source:

```bash
cargo run -p cli -- targets --json
cargo run -p cli -- record start --target display:1 --duration 30s
```

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

Create optimized dev artifacts:

```bash
./scripts/package-macos.sh release
./scripts/package-cli-macos.sh release
```

The app package contains `wrec-app`, `daemon`, and `capture-engine`. The CLI
package contains `wrec`, `daemon`, and `capture-engine`, so it can run without
copying anything out of the app bundle.

Pushing a `v*` tag whose commit is on `main` runs the release workflow and
uploads the dev `.dmg` and dev CLI archive to GitHub Releases.
