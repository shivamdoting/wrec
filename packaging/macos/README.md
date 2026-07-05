# macOS packaging

`scripts/package-macos.sh` assembles the macOS app bundle:

```text
Wrec*.app/
  Contents/
    Info.plist
    MacOS/
      wrec-app
      daemon
      capture-engine
    Resources/
```

The packaged app resolves `daemon` beside its executable. The daemon resolves
`capture-engine` beside its executable at runtime. Cargo development still falls
back to the capture-engine path emitted by `crates/macos/build.rs`.

For contributor/dev packaging:

```bash
./scripts/package-macos.sh
```

This creates `dist/dev/Wrec Dev.app` with the dev Cargo profile, ad-hoc
signing, bundle id `app.wrec.dev`, shared app data in
`~/Library/Application Support/Wrec`, and recordings in `~/Movies/Wrec Dev`.
It also writes `dist/dev/README.md` on every run with the local commands and
build details for that generated app.

Dev packaging uses `images/wrec-dev.png` as the app icon.

For release packaging:

```bash
./scripts/package-macos.sh release
```

This creates `dist/release/Wrec Dev.app` with the release Cargo profile,
bundle id `app.wrec.dev`, and a GitHub-facing dev DMG like
`dist/release/wrec-0.1.0-dev.dmg` by default. Release packaging does not
generate the companion README.

Release packaging uses `images/wrec-dev.png` as the app icon because GitHub
artifacts are dev-labelled.

GitHub artifacts are unsigned dev builds. The app bundle is ad-hoc signed so
the bundle is internally consistent, but macOS Gatekeeper will still warn users
because there is no Developer ID signature or notarization.

Set `ICON_SOURCE=/path/to/icon.png` to override the channel's default icon.

## CLI packaging

`scripts/package-cli-macos.sh` assembles the standalone CLI runtime:

```text
wrec-cli/
  wrec
  daemon
  capture-engine
```

The resulting dev archive is written to
`dist/cli/wrec-cli-<target>-dev.tar.gz`.
`scripts/install-cli.sh` installs that runtime under `/usr/local/lib/wrec` and
places a managed wrapper at `/usr/local/bin/wrec`.

This package is intentionally separate from the app bundle. It carries the same
daemon and capture-engine runtime so terminal users and agents can install
`wrec` without copying files out of `Wrec Dev.app`.

## GitHub release workflow

`.github/workflows/release.yml` publishes macOS dev downloads when a `v*`
tag is pushed and the tagged commit is on `origin/main`. GitHub Actions cannot
filter tags by source branch in the trigger itself, so the workflow does an
explicit ancestry check before packaging.

The workflow uploads the unsigned dev `.dmg` and standalone dev CLI runtime
archive as GitHub Release assets. It does not require Apple Developer Program
secrets.
