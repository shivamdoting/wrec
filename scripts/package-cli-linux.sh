#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
  echo 'Build the Linux CLI package on Linux.' >&2
  exit 1
fi
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
cargo build --release --locked -p cli -p daemon
task_target="${CARGO_TARGET_DIR:-$repo_root/target}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
install -Dm755 "$task_target/release/wrec" "$stage/bin/wrec"
install -Dm755 "$task_target/release/daemon" "$stage/lib/wrec/daemon"
install -Dm644 packaging/linux/README.md "$stage/share/doc/wrec/README.md"
install -Dm644 LICENSE "$stage/share/doc/wrec/LICENSE"
mkdir -p dist/cli/linux
archive="$repo_root/dist/cli/linux/wrec-cli-$(uname -m)-unknown-linux-gnu.tar.gz"
tar -czf "$archive" -C "$stage" bin lib share
echo "$archive"
