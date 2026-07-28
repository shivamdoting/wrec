#!/bin/sh
set -eu

PREFIX="${WREC_PREFIX:-/usr/local}"
VERSION="${WREC_VERSION:-latest}"
REPO="${WREC_REPO:-shivamdoting/wrec}"
CHANNEL="${WREC_CHANNEL:-release}"
ARTIFACT_QUALIFIER="${WREC_ARTIFACT_QUALIFIER-}"
BIN_DIR="$PREFIX/bin"

case "$CHANNEL" in
  dev)
    CLI_NAME="wrec-dev"
    ARCHIVE_PREFIX="wrec-dev-cli"
    ;;
  nightly)
    CLI_NAME="wrec-nightly"
    ARCHIVE_PREFIX="wrec-nightly-cli"
    ;;
  release | stable)
    CHANNEL="release"
    CLI_NAME="wrec"
    ARCHIVE_PREFIX="wrec-cli"
    ;;
  *)
    echo "unsupported WREC_CHANNEL: $CHANNEL (expected dev, nightly, or release)" >&2
    exit 1
    ;;
esac

LIB_DIR="$PREFIX/lib/$CLI_NAME"
BIN="$BIN_DIR/$CLI_NAME"
CLI="$LIB_DIR/$CLI_NAME"
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
  elif can_write_prefix; then
    "$@"
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
    x86_64)
      echo "unsupported architecture: Intel Macs are not supported by this release" >&2
      exit 1
      ;;
    *)
      echo "unsupported architecture: $arch" >&2
      exit 1
      ;;
  esac
}

asset_name() {
  target="$(target_name)"
  asset="$ARCHIVE_PREFIX-$target"
  if [ -n "$ARTIFACT_QUALIFIER" ]; then
    asset="$asset-$ARTIFACT_QUALIFIER"
  fi
  echo "$asset.tar.gz"
}

release_url() {
  file="$1"

  if [ "$VERSION" = "latest" ]; then
    if [ "$CHANNEL" = "nightly" ]; then
      echo "https://github.com/$REPO/releases/download/nightly/$file"
    else
      echo "https://github.com/$REPO/releases/latest/download/$file"
    fi
  else
    case "$VERSION" in
      nightly) tag="nightly" ;;
      v*) tag="$VERSION" ;;
      *) tag="v$VERSION" ;;
    esac
    echo "https://github.com/$REPO/releases/download/$tag/$file"
  fi
}

download_url() {
  release_url "$(asset_name)"
}

# Verifies the downloaded archive against the release's published SHA256SUMS
# so nobody has to run shasum by hand. Fails closed on any mismatch or fetch
# failure; only a confirmed 404 (a release that predates SHA256SUMS) installs
# with a loud warning.
verify_archive() {
  sums="$tmp_dir/SHA256SUMS"
  sums_url="$(release_url SHA256SUMS)"
  http_status="$(curl -sSL -o "$sums" -w '%{http_code}' "$sums_url" 2>/dev/null)" || http_status="000"
  if [ "$http_status" = "404" ]; then
    echo "warning: this release publishes no SHA256SUMS; skipping checksum verification" >&2
    return 0
  fi
  if [ "$http_status" != "200" ]; then
    echo "error: could not fetch SHA256SUMS (HTTP $http_status); refusing to install an unverified archive" >&2
    echo "Retry in a moment; if this repeats, download manually and verify against the release page." >&2
    exit 1
  fi

  asset="$(asset_name)"
  expected="$(awk -v name="$asset" '$2 == name { print $1 }' "$sums")"
  if [ -z "$expected" ]; then
    echo "error: $asset is not listed in the release SHA256SUMS; refusing to install" >&2
    exit 1
  fi

  actual="$(shasum -a 256 "$archive" | awk '{ print $1 }')"
  if [ "$actual" != "$expected" ]; then
    cat >&2 <<EOF
error: checksum mismatch for $asset; refusing to install
  expected: $expected
  actual:   $actual
The download may be corrupted or tampered with. Retry, and if this repeats,
report it: https://github.com/$REPO/security
EOF
    exit 1
  fi

  echo "Verified sha256 $actual"
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
stage=""
backup=""
wrapper_stage=""
runtime_swapped=0
committed=0
cleanup() {
  rm -rf "$tmp_dir"
  if [ "$committed" -ne 1 ]; then
    if [ "$runtime_swapped" -eq 1 ] && [ -e "$LIB_DIR" ]; then
      run_root rm -rf "$LIB_DIR" || true
    fi
    if [ -n "$backup" ] && [ -e "$backup" ] && [ ! -e "$LIB_DIR" ]; then
      run_root mv "$backup" "$LIB_DIR" || true
    fi
  fi
  if [ -n "$stage" ]; then
    run_root rm -rf "$stage" || true
  fi
  if [ -n "$wrapper_stage" ]; then
    run_root rm -f "$wrapper_stage" || true
  fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

archive="${WREC_CLI_ARCHIVE:-$tmp_dir/wrec-cli.tar.gz}"
if [ -z "${WREC_CLI_ARCHIVE:-}" ]; then
  url="$(download_url)"
  echo "Downloading $url"
  if ! curl -fL "$url" -o "$archive"; then
    asset="$(asset_name)"
    cat >&2 <<EOF
Could not download the wrec CLI package.
URL: $url

This usually means there is no public GitHub Release asset named $asset.
Publish a v* release from a public repo, set WREC_VERSION to an existing tag, or install from a local archive:
  curl -fsSL https://wrec.app/install | WREC_CLI_ARCHIVE=/path/to/$asset sh
EOF
    exit 1
  fi
  verify_archive
fi

tar -xzf "$archive" -C "$tmp_dir"
payload="$tmp_dir/wrec-cli"

for file in "$CLI_NAME" daemon capture-engine; do
  if [ ! -x "$payload/$file" ]; then
    echo "missing executable in CLI package: $file" >&2
    exit 1
  fi
done
if [ ! -f "$payload/artifact-version" ]; then
  echo "missing artifact-version in CLI package" >&2
  exit 1
fi
ARTIFACT_VERSION="$(cat "$payload/artifact-version")"

wrapper="$tmp_dir/wrec-wrapper"
{
  echo "#!/bin/sh"
  echo "$MARKER"
  echo "export WREC_CHANNEL=\"$CHANNEL\""
  echo "export WREC_ARTIFACT_VERSION=\"$ARTIFACT_VERSION\""
  echo "exec \"$CLI\" \"\$@\""
} >"$wrapper"

lib_parent="$PREFIX/lib"
stage="$lib_parent/.$CLI_NAME.staged-$$"
backup="$lib_parent/.$CLI_NAME.old-$$"
wrapper_stage="$BIN_DIR/.$CLI_NAME.staged-$$"
run_root install -d -m 0755 "$BIN_DIR" "$lib_parent"
run_root rm -rf "$stage" "$backup"
run_root rm -f "$wrapper_stage"
run_root install -d -m 0755 "$stage"
run_root install -m 0755 "$payload/$CLI_NAME" "$stage/$CLI_NAME"
run_root install -m 0755 "$payload/daemon" "$stage/daemon"
run_root install -m 0755 "$payload/capture-engine" "$stage/capture-engine"
run_root install -m 0644 "$payload/artifact-version" "$stage/artifact-version"
run_root install -m 0755 "$wrapper" "$wrapper_stage"

# Swap the complete three-binary runtime as one directory so an interrupted
# update cannot leave a CLI, daemon, and capture engine from mixed releases.
if [ -e "$LIB_DIR" ]; then
  run_root mv "$LIB_DIR" "$backup"
fi
if ! run_root mv "$stage" "$LIB_DIR"; then
  if [ -e "$backup" ] && [ ! -e "$LIB_DIR" ]; then
    run_root mv "$backup" "$LIB_DIR"
  fi
  echo "failed to install wrec runtime; the previous version was restored" >&2
  exit 1
fi
runtime_swapped=1
run_root mv "$wrapper_stage" "$BIN"
committed=1
run_root rm -rf "$backup"

echo "Installed $CHANNEL CLI at $BIN"
echo "Run: $CLI_NAME help"
