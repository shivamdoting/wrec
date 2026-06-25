#!/bin/sh
set -eu

PREFIX="${WREC_PREFIX:-/usr/local}"
VERSION="${WREC_VERSION:-latest}"
REPO="${WREC_REPO:-shivamhwp/wrec}"
ARTIFACT_QUALIFIER="${WREC_ARTIFACT_QUALIFIER-dev}"
BIN_DIR="$PREFIX/bin"
LIB_DIR="$PREFIX/lib/wrec"
BIN="$BIN_DIR/wrec"
CLI="$LIB_DIR/wrec"
DAEMON="$LIB_DIR/daemon"
CAPTURE_ENGINE="$LIB_DIR/capture-engine"
MARKER="# managed by wrec"

can_write_prefix() {
  path="$PREFIX"
  while [ ! -e "$path" ]; do
    parent="$(dirname "$path")"
    [ "$parent" = "$path" ] && return 1
    path="$parent"
  done

  [ -w "$path" ]
}

run_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif can_write_prefix && "$@"; then
    return 0
  else
    sudo "$@"
  fi
}

is_managed_bin() {
  [ -f "$BIN" ] && grep -q "$MARKER" "$BIN" 2>/dev/null
}

target_name() {
  os="$(uname -s)"
  arch="$(uname -m)"

  if [ "$os" != "Darwin" ]; then
    echo "unsupported OS: $os" >&2
    exit 1
  fi

  case "$arch" in
    arm64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *)
      echo "unsupported architecture: $arch" >&2
      exit 1
      ;;
  esac
}

download_url() {
  target="$(target_name)"
  asset="wrec-cli-$target"
  if [ -n "$ARTIFACT_QUALIFIER" ]; then
    asset="$asset-$ARTIFACT_QUALIFIER"
  fi
  asset="$asset.tar.gz"

  if [ "$VERSION" = "latest" ]; then
    echo "https://github.com/$REPO/releases/latest/download/$asset"
  else
    case "$VERSION" in
      v*) tag="$VERSION" ;;
      *) tag="v$VERSION" ;;
    esac
    echo "https://github.com/$REPO/releases/download/$tag/$asset"
  fi
}

if [ "${WREC_UNINSTALL:-0}" = "1" ]; then
  if [ -e "$BIN" ] && ! is_managed_bin; then
    echo "$BIN exists and is not managed by wrec" >&2
    exit 1
  fi

  run_root rm -f "$BIN"
  run_root rm -rf "$LIB_DIR"
  echo "Removed wrec CLI from $BIN"
  exit 0
fi

if [ -e "$BIN" ] && ! is_managed_bin; then
  echo "$BIN exists and is not managed by wrec" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

archive="${WREC_CLI_ARCHIVE:-$tmp_dir/wrec-cli.tar.gz}"
if [ -z "${WREC_CLI_ARCHIVE:-}" ]; then
  url="$(download_url)"
  echo "Downloading $url"
  if ! curl -fL "$url" -o "$archive"; then
    target="$(target_name)"
    cat >&2 <<EOF
Could not download the wrec CLI package.
URL: $url

This usually means there is no GitHub Release asset named $asset.
Publish a v* release, set WREC_VERSION to an existing tag, or install from a local archive:
  curl -fsSL https://wrec-beta.vercel.app/install | WREC_CLI_ARCHIVE=/path/to/$asset sh
EOF
    exit 1
  fi
fi

tar -xzf "$archive" -C "$tmp_dir"
payload="$tmp_dir/wrec-cli"

for file in wrec daemon capture-engine; do
  if [ ! -x "$payload/$file" ]; then
    echo "missing executable in CLI package: $file" >&2
    exit 1
  fi
done

wrapper="$tmp_dir/wrec-wrapper"
{
  echo "#!/bin/sh"
  echo "$MARKER"
  echo "exec \"$CLI\" \"\$@\""
} >"$wrapper"

run_root install -d -m 0755 "$BIN_DIR" "$LIB_DIR"
run_root install -m 0755 "$payload/wrec" "$CLI"
run_root install -m 0755 "$payload/daemon" "$DAEMON"
run_root install -m 0755 "$payload/capture-engine" "$CAPTURE_ENGINE"
run_root install -m 0755 "$wrapper" "$BIN"

echo "Installed wrec CLI at $BIN"
echo "Run: wrec help"
