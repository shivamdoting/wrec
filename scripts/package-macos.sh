#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log() {
  printf '[wrec-package] %s\n' "$*"
}

die() {
  printf '[wrec-package] error: %s\n' "$*" >&2
  exit 1
}

run() {
  log "+ $*"
  "$@"
}

generate_app_icon() {
  local source="$1"
  local iconset="$RESOURCES/AppIcon.iconset"
  local output="$RESOURCES/AppIcon.icns"

  if [[ ! -f "$source" ]]; then
    die "App icon source not found: $source"
  fi

  log "Generating app icon from: $source"
  run rm -rf "$iconset"
  run mkdir -p "$iconset"

  local names=(
    icon_16x16.png
    icon_16x16@2x.png
    icon_32x32.png
    icon_32x32@2x.png
    icon_128x128.png
    icon_128x128@2x.png
    icon_256x256.png
    icon_256x256@2x.png
    icon_512x512.png
    icon_512x512@2x.png
  )
  local sizes=(16 32 32 64 128 256 256 512 512 1024)

  for i in "${!names[@]}"; do
    local size="${sizes[$i]}"
    run cp "$source" "$iconset/${names[$i]}"
    run sips -Z "$size" --padToHeightWidth "$size" "$size" "$iconset/${names[$i]}" >/dev/null
  done

  run iconutil -c icns "$iconset" -o "$output"
  run rm -rf "$iconset"
  run /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$INFO_PLIST" 2>/dev/null \
    || run /usr/libexec/PlistBuddy -c "Set :CFBundleIconFile AppIcon" "$INFO_PLIST"
}

write_dev_readme() {
  local readme="$DIST_DIR/README.md"

  log "Writing dev README: $readme"
  cat >"$readme" <<EOF
# Wrec Dev

This directory contains the latest local contributor build of Wrec.

## Open the app

\`\`\`bash
open "$APP_NAME.app"
\`\`\`

## Rebuild this dev app

From the repository root:

\`\`\`bash
./scripts/package-macos.sh
\`\`\`

That command rebuilds the debug-profile app, recreates this app bundle from
scratch, copies the current \`wrec-app\`, \`daemon\`, and \`capture-engine\`
binaries, signs them ad-hoc, and verifies the app signature.

## Run the bundled daemon

\`\`\`bash
"$APP_NAME.app/Contents/MacOS/daemon"
\`\`\`

## Release packaging

Release packaging is explicit:

\`\`\`bash
./scripts/package-macos.sh release
\`\`\`

Release builds use the release Cargo profile and the public bundle id.

## Current build

- Channel: \`$CHANNEL\`
- Version: \`$VERSION\`
- Git SHA: \`$GIT_SHA\`
- Built at: \`$BUILT_AT\`
- Built by: \`$BUILT_BY\`
- Host: \`$BUILD_HOST\`
- App: \`$APP_NAME.app\`
- Bundle id: \`$BUNDLE_ID\`
- Cargo profile: \`$PROFILE_DIR\`

## Dev app paths

- App data: \`~/Library/Application Support/$APP_NAME\`
- Default recordings: \`~/Movies/$APP_NAME\`
- Logs: \`~/Library/Application Support/$APP_NAME/wrec.log\`
EOF
}

usage() {
  cat <<EOF
Usage: $0 [dev|nightly|release]

Defaults to dev. Dev builds use the debug Cargo profile, ad-hoc signing, and
create "Wrec Dev.app". Release builds use --release and create "Wrec.app".
EOF
}

CHANNEL="${1:-${WREC_CHANNEL:-dev}}"
if [[ $# -gt 1 ]]; then
  usage >&2
  exit 1
fi

case "$CHANNEL" in
  dev | nightly)
    CHANNEL="dev"
    DEFAULT_APP_NAME="Wrec Dev"
    DEFAULT_BUNDLE_ID="app.wrec.wrec.dev"
    DEFAULT_PROFILE="dev"
    DEFAULT_CREATE_DMG="0"
    DEFAULT_ICON_SOURCE="$ROOT/images/wrec-dev.png"
    ;;
  release)
    DEFAULT_APP_NAME="Wrec"
    DEFAULT_BUNDLE_ID="app.wrec.wrec"
    DEFAULT_PROFILE="release"
    DEFAULT_CREATE_DMG="1"
    DEFAULT_ICON_SOURCE="$ROOT/images/wrec.png"
    ;;
  -h | --help | help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 1
    ;;
esac

APP_NAME="${APP_NAME:-$DEFAULT_APP_NAME}"
BIN_NAME="${BIN_NAME:-wrec-app}"
DAEMON_BIN_NAME="${DAEMON_BIN_NAME:-daemon}"
BUNDLE_ID="${BUNDLE_ID:-$DEFAULT_BUNDLE_ID}"
PROFILE="${PROFILE:-$DEFAULT_PROFILE}"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
NOTARIZE="${NOTARIZE:-0}"
CREATE_DMG="${CREATE_DMG:-$DEFAULT_CREATE_DMG}"
ICON_SOURCE="${ICON_SOURCE:-$DEFAULT_ICON_SOURCE}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
DIST_DIR="$ROOT/dist/$CHANNEL"
APP="$DIST_DIR/$APP_NAME.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
INFO_PLIST="$CONTENTS/Info.plist"
ENTITLEMENTS="$ROOT/packaging/macos/entitlements.plist"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/crates/app/Cargo.toml" | head -n 1)}"
GIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo local)"
BUILT_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
BUILT_BY="$(id -un 2>/dev/null || whoami 2>/dev/null || echo unknown)"
BUILD_HOST="$(hostname 2>/dev/null || echo unknown)"
ARTIFACT_VERSION="${ARTIFACT_VERSION:-$VERSION}"

case "$CHANNEL" in
  dev)
    ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER:-dev-$GIT_SHA}"
    ;;
  release)
    ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER-dev}"
    ;;
esac

if [[ -n "${ARTIFACT_QUALIFIER:-}" ]]; then
  ARTIFACT_VERSION="${ARTIFACT_VERSION}-${ARTIFACT_QUALIFIER}"
fi

DMG_NAME="${DMG_NAME:-wrec-$ARTIFACT_VERSION.dmg}"

if [[ "$NOTARIZE" == "1" && "$CODESIGN_IDENTITY" == "-" ]]; then
  die "NOTARIZE=1 requires CODESIGN_IDENTITY to be a Developer ID Application identity"
fi

case "$PROFILE" in
  dev | debug)
    PROFILE_DIR="debug"
    cargo_args=(build)
    ;;
  release)
    PROFILE_DIR="release"
    cargo_args=(build --release)
    ;;
  *)
    die "Unsupported PROFILE: $PROFILE"
    ;;
esac

log "Packaging channel: $CHANNEL"
log "App name: $APP_NAME"
log "Bundle id: $BUNDLE_ID"
log "Cargo profile: $PROFILE_DIR"
log "Version: $VERSION"
log "Artifact version: $ARTIFACT_VERSION"
log "Output app: $APP"
log "Icon source: $ICON_SOURCE"
log "Dmg enabled: $CREATE_DMG"
log "Dmg name: $DMG_NAME"
log "Notarization enabled: $NOTARIZE"

log "Building Rust app"
run cargo "${cargo_args[@]}" -p app --bin "$BIN_NAME"
log "Building daemon and capture engine"
run cargo "${cargo_args[@]}" -p daemon --bin "$DAEMON_BIN_NAME"

CAPTURE_ENGINE=""
if [[ -d "$TARGET_DIR/$PROFILE_DIR/build" ]]; then
  CAPTURE_ENGINE="$(find "$TARGET_DIR/$PROFILE_DIR/build" -path "*/out/capture-engine" -type f -print | sort | tail -n 1)"
fi
if [[ -z "$CAPTURE_ENGINE" ]]; then
  die "Could not find compiled capture-engine in $TARGET_DIR/$PROFILE_DIR/build"
fi

if [[ ! -f "$TARGET_DIR/$PROFILE_DIR/$BIN_NAME" ]]; then
  die "Could not find compiled app binary at $TARGET_DIR/$PROFILE_DIR/$BIN_NAME"
fi
if [[ ! -f "$TARGET_DIR/$PROFILE_DIR/$DAEMON_BIN_NAME" ]]; then
  die "Could not find compiled daemon binary at $TARGET_DIR/$PROFILE_DIR/$DAEMON_BIN_NAME"
fi

log "Using capture engine: $CAPTURE_ENGINE"
log "Recreating app bundle from scratch"
if [[ "$CHANNEL" == "dev" ]]; then
  log "Clearing previous dev build directory"
  run rm -rf "$DIST_DIR"
else
  run rm -rf "$APP"
fi
run mkdir -p "$MACOS" "$RESOURCES"

log "Copying executables and metadata"
run cp "$TARGET_DIR/$PROFILE_DIR/$BIN_NAME" "$MACOS/$BIN_NAME"
run cp "$TARGET_DIR/$PROFILE_DIR/$DAEMON_BIN_NAME" "$MACOS/$DAEMON_BIN_NAME"
run cp "$CAPTURE_ENGINE" "$MACOS/capture-engine"
run cp "$ROOT/packaging/macos/Info.plist" "$INFO_PLIST"
generate_app_icon "$ICON_SOURCE"

log "Writing bundle metadata"
run /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$INFO_PLIST"
run /usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName $APP_NAME" "$INFO_PLIST"
run /usr/libexec/PlistBuddy -c "Set :CFBundleName $APP_NAME" "$INFO_PLIST"
run /usr/libexec/PlistBuddy -c "Set :CFBundleExecutable $BIN_NAME" "$INFO_PLIST"
run /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$INFO_PLIST"
run /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$INFO_PLIST"

sign_args=(--force --options runtime --sign "$CODESIGN_IDENTITY")
if [[ "$CODESIGN_IDENTITY" != "-" ]]; then
  sign_args+=(--timestamp)
fi

log "Signing capture engine, daemon, and app"
run codesign "${sign_args[@]}" "$MACOS/capture-engine"
run codesign "${sign_args[@]}" "$MACOS/$DAEMON_BIN_NAME"
run codesign "${sign_args[@]}" --entitlements "$ENTITLEMENTS" "$APP"
log "Verifying app signature"
run codesign --verify --deep --strict --verbose=2 "$APP"

if [[ "$CHANNEL" == "dev" ]]; then
  write_dev_readme
fi

if [[ "$CREATE_DMG" == "1" ]]; then
  DMG="$DIST_DIR/$DMG_NAME"
  log "Creating dmg: $DMG"
  run rm -f "$DIST_DIR"/*.dmg
  run hdiutil create -volname "$APP_NAME" -srcfolder "$APP" -ov -format UDZO "$DMG"
  log "Created dmg: $DMG"

  if [[ "$NOTARIZE" == "1" ]]; then
    : "${APPLE_ID:?APPLE_ID is required for notarization}"
    : "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required for notarization}"
    : "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD is required for notarization}"

    log "Submitting dmg for notarization"
    xcrun notarytool submit "$DMG" \
      --apple-id "$APPLE_ID" \
      --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" \
      --wait
    log "Stapling notarization ticket"
    run xcrun stapler staple "$DMG"
    log "Verifying Gatekeeper acceptance"
    run spctl -a -vv -t open --context context:primary-signature "$DMG"
    log "Notarized dmg is ready: $DMG"
  fi
else
  log "Dmg disabled for this channel"
  log "Created app: $APP"
fi
