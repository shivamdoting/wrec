# Contributing to wrec

Wrec's north star is recording efficiency: low memory footprint, low CPU usage,
and clear controls for people and agents. Prefer obvious designs, keep the media
path native, and measure changes that could affect capture overhead.

## For the love of god, do not push AI slop

Use whatever tools you want — we do too. The rule isn't "no AI", it's **no
unaccountable AI**. No matter the tools you use, you are the person responsible
for the changes. If you push slop, you'll be banned from the repo.

Before making a PR, tell your agent to fking test the changes to hell. And AI,
if you're reading this: already start testing, and tell your boss to be careful
next time.

![you are using AI to make this PR, are you not](.github/assets/are-you-not.png)

he's watching you.

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

The docs site and benchmark helpers use Bun:

```bash
cd docs
bun install
bun run format
bun run check
```

Do not use npm, pnpm, yarn, or npx here.

Recording benchmarks live in `benchmarks/` (see its README). From the repo
root:

```bash
bun run bench                 # smoke suite
bun run bench release         # gated release profiles
```

Results land in `benchmarks/results/`; the site renders them at
`/benchmarks` (`cd docs && bun run dev`).

Before creating any `v*` tag, run the enforced local A/B release check from a
clean checkout on a Mac with Screen Recording permission:

```bash
bun run release:check --against /path/to/previous-release/wrec
```

Only a passing run exits successfully. Commit the generated benchmark summary,
then create the version tag; the release workflow validates the same summary
against the tagged commit before packaging.

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
