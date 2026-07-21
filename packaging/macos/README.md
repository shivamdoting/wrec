# macOS packaging

`scripts/package-macos.sh` assembles the macOS app bundle:

```text
Wrec*.app/
  Contents/
    Info.plist
    MacOS/
      wrec-app          # SwiftUI menu bar shell (built from apps/mac)
      daemon
      capture-engine
    Resources/
      wrec-mac_wrec-app.bundle   # Swift resources (fonts, SKILL.md)
```

`wrec-app` is the native SwiftUI shell built with `swift build` from
`apps/mac`; the app is a menu bar item (`LSUIElement`), not a windowed app.
The packaged app resolves `daemon` beside its executable. The daemon resolves
`capture-engine` beside its executable at runtime. Cargo development still falls
back to the capture-engine path emitted by `crates/macos/build.rs`.

Smoke-test any built shell headlessly (drives daemon spawn, targets, a real
2-second recording) with:

```bash
WREC_SMOKE=1 "dist/dev/Wrec Dev.app/Contents/MacOS/wrec-app"
```

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

This creates `dist/release/Wrec.app` with the release Cargo profile, bundle id
`app.wrec.mac`, and a DMG like `dist/release/wrec-0.1.0.dmg`. Release
packaging does not generate the companion README.

Release packaging uses `images/wrec-icon.png` as the app icon; dev packaging
uses the DEV-badged `images/wrec-dev.png`.

GitHub artifacts are ad-hoc signed so the bundle is internally consistent, but
macOS Gatekeeper will still warn users on the app DMG because there is no
Developer ID signature or notarization. The CLI installer is not affected by
the warning.

Set `ICON_SOURCE=/path/to/icon.png` to override the channel's default icon.

## CLI packaging

`scripts/package-cli-macos.sh` assembles the standalone CLI runtime:

```text
wrec-cli/
  wrec
  daemon
  capture-engine
```

The resulting archive is written to `dist/cli/wrec-cli-<target>.tar.gz`.
`scripts/install-cli.sh` installs that runtime under `/usr/local/lib/wrec` and
places a managed wrapper at `/usr/local/bin/wrec`.

This package is intentionally separate from the app bundle. It carries the same
daemon and capture-engine runtime so terminal users and agents can install
`wrec` without copying files out of the app bundle.

## GitHub release workflow

`.github/workflows/release.yml` publishes macOS downloads when a `v*`
tag is pushed and the tagged commit is on `origin/main`. GitHub Actions cannot
filter tags by source branch in the trigger itself, so the workflow does an
explicit ancestry check before packaging.

The workflow uploads the unsigned `.dmg` and standalone CLI runtime archive as
GitHub Release assets. It does not require Apple Developer Program secrets. The
app is distributed from GitHub Releases and the CLI from the curl installer, so
no further publishing steps are needed.
