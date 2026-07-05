#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

log() {
  printf '[wrec-homebrew] %s\n' "$*"
}

die() {
  printf '[wrec-homebrew] error: %s\n' "$*" >&2
  exit 1
}

VERSION="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)}"
VERSION="${VERSION#v}"
[[ -n "$VERSION" ]] || die "could not determine version"

BASE="https://github.com/shivamhwp/wrec/releases/download/v$VERSION"
CASK="$ROOT/packaging/homebrew/Casks/wrec.rb"
FORMULA="$ROOT/packaging/homebrew/Formula/wrec-cli.rb"

asset_sha() {
  local url="$1"
  log "Fetching $url" >&2
  curl -fL --retry 3 "$url" | shasum -a 256 | awk '{print $1}'
}

DMG_SHA="$(asset_sha "$BASE/wrec-$VERSION.dmg")"
CLI_SHA="$(asset_sha "$BASE/wrec-cli-aarch64-apple-darwin.tar.gz")"

sed -i '' \
  -e "s/^  version \".*\"/  version \"$VERSION\"/" \
  -e "s/^  sha256 \"[0-9a-f]*\"/  sha256 \"$DMG_SHA\"/" \
  "$CASK"
sed -i '' \
  -e "s/^  version \".*\"/  version \"$VERSION\"/" \
  -e "s/^  sha256 \"[0-9a-f]*\"/  sha256 \"$CLI_SHA\"/" \
  "$FORMULA"

log "Updated $CASK (sha256 $DMG_SHA)"
log "Updated $FORMULA (sha256 $CLI_SHA)"
log "Copy packaging/homebrew/{Casks,Formula} into the shivamhwp/homebrew-tap repo and push."
