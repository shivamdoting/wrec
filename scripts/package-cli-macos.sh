#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log() {
  printf '[wrec-cli-package] %s\n' "$*"
}

die() {
  printf '[wrec-cli-package] error: %s\n' "$*" >&2
  exit 1
}

run() {
  log "+ $*"
  "$@"
}

target_triple() {
  case "$(uname -m)" in
    arm64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *) die "unsupported architecture: $(uname -m)" ;;
  esac
}

usage() {
  cat <<EOF
Usage: $0 [dev|release]

Defaults to dev. The package contains wrec, daemon, and capture-engine.
EOF
}

CHANNEL="${1:-${WREC_CHANNEL:-dev}}"
if [[ $# -gt 1 ]]; then
  usage >&2
  exit 1
fi

case "$CHANNEL" in
  dev)
    PROFILE_DIR="debug"
    cargo_args=(build)
    ;;
  release)
    PROFILE_DIR="release"
    cargo_args=(build --release)
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

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
DIST_DIR="$ROOT/dist/cli"
STAGE="$DIST_DIR/wrec-cli"
TARGET="$(target_triple)"
case "$CHANNEL" in
  release) ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER-dev}" ;;
  *) ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER:-}" ;;
esac
ARCHIVE_SUFFIX=""
if [[ -n "${ARTIFACT_QUALIFIER:-}" ]]; then
  ARCHIVE_SUFFIX="-$ARTIFACT_QUALIFIER"
fi
ARCHIVE="$DIST_DIR/wrec-cli-$TARGET$ARCHIVE_SUFFIX.tar.gz"

log "Packaging channel: $CHANNEL"
log "Cargo profile: $PROFILE_DIR"
log "Target: $TARGET"
log "Archive: $ARCHIVE"

log "Building CLI"
run cargo "${cargo_args[@]}" -p cli --bin wrec
log "Building daemon and capture engine"
run cargo "${cargo_args[@]}" -p daemon --bin daemon

CAPTURE_ENGINE=""
if [[ -d "$TARGET_DIR/$PROFILE_DIR/build" ]]; then
  CAPTURE_ENGINE="$(find "$TARGET_DIR/$PROFILE_DIR/build" -path "*/out/capture-engine" -type f -print | sort | tail -n 1)"
fi
if [[ -z "$CAPTURE_ENGINE" ]]; then
  die "Could not find compiled capture-engine in $TARGET_DIR/$PROFILE_DIR/build"
fi

for file in "$TARGET_DIR/$PROFILE_DIR/wrec" "$TARGET_DIR/$PROFILE_DIR/daemon"; do
  if [[ ! -f "$file" ]]; then
    die "Missing executable: $file"
  fi
done

run rm -rf "$STAGE"
run mkdir -p "$STAGE"
run cp "$TARGET_DIR/$PROFILE_DIR/wrec" "$STAGE/wrec"
run cp "$TARGET_DIR/$PROFILE_DIR/daemon" "$STAGE/daemon"
run cp "$CAPTURE_ENGINE" "$STAGE/capture-engine"

run rm -f "$DIST_DIR"/wrec-cli-"$TARGET"*.tar.gz
run tar -C "$DIST_DIR" -czf "$ARCHIVE" wrec-cli
log "Created $ARCHIVE"
