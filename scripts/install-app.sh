#!/bin/sh
set -eu

# Installs the Wrec app into /Applications from GitHub Releases. Because the
# download happens over curl, macOS never applies the quarantine attribute,
# so the app opens without the Gatekeeper "damaged/unverified" prompt that a
# browser download triggers.

APPS_DIR="${WREC_APPS_DIR:-/Applications}"
VERSION="${WREC_VERSION:-latest}"
REPO="${WREC_REPO:-shivamdoting/wrec}"
CHANNEL="${WREC_CHANNEL:-release}"
ARTIFACT_QUALIFIER="${WREC_ARTIFACT_QUALIFIER-}"

case "$CHANNEL" in
  nightly) ARCHIVE_PREFIX="wrec-nightly-app" ;;
  release | stable)
    CHANNEL="release"
    ARCHIVE_PREFIX="wrec-app"
    ;;
  *)
    echo "unsupported WREC_CHANNEL: $CHANNEL (expected nightly or release)" >&2
    exit 1
    ;;
esac

can_write_apps_dir() {
  path="$APPS_DIR"
  while [ ! -e "$path" ]; do
    parent="$(dirname "$path")"
    [ "$parent" = "$path" ] && return 1
    path="$parent"
  done

  [ -w "$path" ]
}

run_root() {
  # Pick the executor by permission up front; retrying a failed command under
  # sudo would prompt for a password and run its side effects twice.
  if [ "$(id -u)" -eq 0 ] || can_write_apps_dir; then
    "$@"
  else
    sudo "$@"
  fi
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

tmp_dir="$(mktemp -d)"
stage=""
backup=""
committed=0
cleanup() {
  rm -rf "$tmp_dir"
  if [ "$committed" -ne 1 ] && [ -n "$backup" ] && [ -e "$backup" ] && [ ! -e "$dest" ]; then
    run_root mv "$backup" "$dest" || true
  fi
  if [ -n "$stage" ]; then
    run_root rm -rf "$stage" || true
  fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

archive="${WREC_APP_ARCHIVE:-$tmp_dir/wrec-app.tar.gz}"
if [ -z "${WREC_APP_ARCHIVE:-}" ]; then
  url="$(release_url "$(asset_name)")"
  echo "Downloading $url"
  if ! curl -fL "$url" -o "$archive"; then
    asset="$(asset_name)"
    cat >&2 <<EOF
Could not download the wrec app package.
URL: $url

This usually means there is no public GitHub Release asset named $asset.
Publish a v* release from a public repo, set WREC_VERSION to an existing tag, or install from a local archive:
  curl -fsSL https://wrec.app/install-app | WREC_APP_ARCHIVE=/path/to/$asset sh
EOF
    exit 1
  fi
  verify_archive
fi

tar -xzf "$archive" -C "$tmp_dir"

bundle=""
for candidate in "$tmp_dir"/*.app; do
  if [ -x "$candidate/Contents/MacOS/wrec-app" ]; then
    bundle="$candidate"
    break
  fi
done
if [ -z "$bundle" ]; then
  echo "archive contains no wrec app bundle" >&2
  exit 1
fi

app_name="$(basename "$bundle" .app)"
dest="$APPS_DIR/$app_name.app"
stage="$APPS_DIR/.$app_name.app.staged-$$"
backup="$APPS_DIR/.$app_name.app.old-$$"

if pgrep -qf "$dest/Contents/MacOS/wrec-app"; then
  echo "Quitting running $app_name"
  osascript -e "tell application \"$app_name\" to quit" >/dev/null 2>&1 || true
  sleep 1
fi

run_root install -d -m 0755 "$APPS_DIR"
run_root rm -rf "$stage" "$backup"
# Stage and verify the complete replacement before the installed app moves.
# ditto preserves the code signature and bundle metadata.
run_root ditto "$bundle" "$stage"
/usr/bin/codesign --verify --deep --strict "$stage"
# curl downloads carry no quarantine flag; clear anything that slipped in
# anyway (e.g. an archive that was originally fetched with a browser).
run_root xattr -dr com.apple.quarantine "$stage" 2>/dev/null || true

if [ -e "$dest" ]; then
  run_root mv "$dest" "$backup"
fi
if ! run_root mv "$stage" "$dest"; then
  if [ -e "$backup" ] && [ ! -e "$dest" ]; then
    run_root mv "$backup" "$dest"
  fi
  echo "failed to install $app_name; the previous app was restored" >&2
  exit 1
fi
committed=1
run_root rm -rf "$backup"

echo "Installed $dest"
echo "Run: open \"$dest\""
