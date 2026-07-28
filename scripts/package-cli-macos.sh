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
Usage: $0 [dev|nightly|release]

Defaults to dev. Each channel gets its own CLI name and artifact directory.
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
    CLI_NAME="wrec-dev"
    ARCHIVE_PREFIX="wrec-dev-cli"
    ;;
  nightly)
    PROFILE_DIR="release"
    cargo_args=(build --release)
    CLI_NAME="wrec-nightly"
    ARCHIVE_PREFIX="wrec-nightly-cli"
    ;;
  release)
    PROFILE_DIR="release"
    cargo_args=(build --release)
    CLI_NAME="wrec"
    ARCHIVE_PREFIX="wrec-cli"
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

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/wrec-$CHANNEL}"
DIST_DIR="$ROOT/dist/cli/$CHANNEL"
STAGE="$DIST_DIR/wrec-cli"
TARGET="$(target_triple)"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)}"
GIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo local)"
if [[ -n "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]]; then
  GIT_SHA="$GIT_SHA-dirty"
fi
case "$CHANNEL" in
  dev) ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER:-dev-$GIT_SHA}" ;;
  nightly) ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER:-nightly-$GIT_SHA}" ;;
  release) ARTIFACT_QUALIFIER="${ARTIFACT_QUALIFIER:-}" ;;
esac
ARCHIVE_SUFFIX=""
if [[ -n "${ARTIFACT_QUALIFIER:-}" ]]; then
  ARCHIVE_SUFFIX="-$ARTIFACT_QUALIFIER"
fi
ARCHIVE="$DIST_DIR/$ARCHIVE_PREFIX-$TARGET$ARCHIVE_SUFFIX.tar.gz"

log "Packaging channel: $CHANNEL"
log "Cargo profile: $PROFILE_DIR"
log "Cargo target: $TARGET_DIR"
log "CLI name: $CLI_NAME"
log "Target: $TARGET"
log "Archive: $ARCHIVE"

log "Building CLI"
run env CARGO_TARGET_DIR="$TARGET_DIR" cargo "${cargo_args[@]}" -p cli --bin wrec
log "Building daemon and capture engine"
cargo_messages="$(mktemp)"
trap 'rm -f "$cargo_messages"' EXIT
log "+ env CARGO_TARGET_DIR=$TARGET_DIR cargo ${cargo_args[*]} -p daemon --bin daemon --message-format=json-render-diagnostics"
CARGO_TARGET_DIR="$TARGET_DIR" cargo "${cargo_args[@]}" -p daemon --bin daemon \
  --message-format=json-render-diagnostics >"$cargo_messages"
CAPTURE_ENGINE="$(
  sed -n 's/.*\["WREC_CAPTURE_ENGINE_PATH","\([^"]*\)"\].*/\1/p' "$cargo_messages" \
    | tail -n 1
)"
if [[ ! -f "$CAPTURE_ENGINE" ]]; then
  die "Cargo did not report the capture-engine built for this daemon"
fi
if ! file "$CAPTURE_ENGINE" | grep -q "Mach-O 64-bit executable $(uname -m)"; then
  die "Capture engine is not a Mach-O executable for $(uname -m): $CAPTURE_ENGINE"
fi

for file in "$TARGET_DIR/$PROFILE_DIR/wrec" "$TARGET_DIR/$PROFILE_DIR/daemon"; do
  if [[ ! -f "$file" ]]; then
    die "Missing executable: $file"
  fi
done

run rm -rf "$STAGE"
run mkdir -p "$STAGE"
run cp "$TARGET_DIR/$PROFILE_DIR/wrec" "$STAGE/$CLI_NAME"
run cp "$TARGET_DIR/$PROFILE_DIR/daemon" "$STAGE/daemon"
run cp "$CAPTURE_ENGINE" "$STAGE/capture-engine"
printf '%s\n' "$VERSION$ARCHIVE_SUFFIX" >"$STAGE/artifact-version"

run rm -f "$DIST_DIR"/"$ARCHIVE_PREFIX"-"$TARGET"*.tar.gz
run tar -C "$DIST_DIR" -czf "$ARCHIVE" wrec-cli
log "Created $ARCHIVE"
