#!/usr/bin/env bash
set -euo pipefail

# Runs the REAL in-place update flow against the local dev build: packages
# dist/dev/Wrec Dev.app, archives it, points the app's mock hooks at the
# archive, and opens the app. About -> "Update to <version>" then performs
# the actual pipeline: extract, validate, daemon stop, bundle swap, relaunch.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-9.9.9}"
MOCK_DIR="$HOME/Library/Application Support/Wrec Dev"
ARCHIVE="$ROOT/dist/dev/wrec-app-mock-update.tar.gz"

log() {
  printf '[wrec-preview-update] %s\n' "$*"
}

log "Packaging the dev app"
"$ROOT/scripts/package-macos.sh"

log "Archiving dist/dev/Wrec Dev.app"
rm -f "$ARCHIVE"
tar -C "$ROOT/dist/dev" -czf "$ARCHIVE" "Wrec Dev.app"

mkdir -p "$MOCK_DIR"
printf '%s\n' "$VERSION" >"$MOCK_DIR/mock-latest-version"
printf '%s\n' "$ARCHIVE" >"$MOCK_DIR/mock-latest-archive"

log "Opening the dev app"
open "$ROOT/dist/dev/Wrec Dev.app"

log "In the app: About -> \"Update to $VERSION\" runs the real update and relaunches."
log "The relaunched app still shows the mock; clean up with:"
log "  rm \"$MOCK_DIR/mock-latest-version\" \"$MOCK_DIR/mock-latest-archive\""
