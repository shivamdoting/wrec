#!/usr/bin/env bash
# build.sh WORKDIR [patched|stock]: build KWin's screencast plugin from Ubuntu's
# kwin 4:6.6.6-0ubuntu0.1 source in WORKDIR (put there by fetch.sh), against
# the installed libkwin6 and kwin-dev of exactly that version. `patched` (the
# default) adds the capture-time timestamp backport; `stock` builds the same
# source unchanged, as a control.
#
# Writes only WORKDIR/{src,cmake,build,plugin}-VARIANT and fails if any of
# them exists; it never deletes anything. The plugin lands in
# WORKDIR/plugin-VARIANT/kwin/plugins/screencast.so, ready for opt-in.sh.
# Nothing is installed. Runs at nice 19 with JOBS (default 2) compile jobs.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=${1:?usage: build.sh WORKDIR [patched|stock]}
variant=${2:-patched}
version=4:6.6.6-0ubuntu0.1
[[ $variant == patched || $variant == stock ]] || { echo "build.sh: variant is patched or stock" >&2; exit 1; }
[[ -d $work && -O $work ]] || { echo "build.sh: $work must be an existing directory you own" >&2; exit 1; }
work=$(realpath "$work")
[[ $(dpkg --print-architecture) == amd64 ]] || { echo "build.sh: tested on amd64 only" >&2; exit 1; }
for package in libkwin6 kwin-dev kwin-common kwin-wayland; do
  got=$(dpkg-query -W -f '${Version}' "$package" 2>/dev/null || true)
  [[ $got == "$version" ]] || { echo "build.sh: $package is '${got:-not installed}', this backport is for $version only" >&2; exit 1; }
done
(cd "$here" && sha256sum -c --quiet --ignore-missing SHA256SUMS)
(cd "$work" && grep -E 'kwin_6\.6\.6' "$here/SHA256SUMS" | sha256sum -c --quiet -) \
  || { echo "build.sh: run fetch.sh $work first" >&2; exit 1; }
for dir in src cmake build plugin; do
  [[ ! -e $work/$dir-$variant ]] || { echo "build.sh: $work/$dir-$variant exists; use a new WORKDIR or move it away" >&2; exit 1; }
done
src=$work/src-$variant
mkdir "$src" "$work/cmake-$variant" "$work/build-$variant"
tar -xJf "$work/kwin_6.6.6.orig.tar.xz" -C "$src" --strip-components=1
tar -xJf "$work/kwin_6.6.6-0ubuntu0.1.debian.tar.xz" -C "$src"
while read -r name; do
  case $name in ''|'#'*) continue ;; esac
  patch -d "$src" -p1 --forward --fuzz=0 --no-backup-if-mismatch < "$src/debian/patches/$name"
done < "$src/debian/patches/series"
if [[ $variant == patched ]]; then
  patch -d "$src" -p1 --forward --fuzz=0 --no-backup-if-mismatch < "$here/kwin-6.6.6-screencast-capture-time-pts.diff"
fi
cp "$here/CMakeLists.txt" "$work/cmake-$variant/"
nice -n19 cmake -S "$work/cmake-$variant" -B "$work/build-$variant" -DCMAKE_BUILD_TYPE=RelWithDebInfo \
  -DKWIN_SOURCE_DIR="$src" -DCMAKE_INSTALL_PREFIX="$work/build-$variant/unused-prefix" \
  > "$work/build-$variant/cmake.log" 2>&1 \
  || { echo "build.sh: cmake failed, see $work/build-$variant/cmake.log" >&2; exit 1; }
nice -n19 cmake --build "$work/build-$variant" -j "${JOBS:-2}" > "$work/build-$variant/build.log" 2>&1 \
  || { echo "build.sh: build failed, see $work/build-$variant/build.log" >&2; exit 1; }
built=$(find "$work/build-$variant" -name screencast.so -type f)
[[ $(printf '%s\n' "$built" | wc -l) == 1 && -n $built ]]
# opt-in.sh accepts only a plugin nobody else can change.
install -d -m 0755 "$work/plugin-$variant" "$work/plugin-$variant/kwin" "$work/plugin-$variant/kwin/plugins"
install -m 0644 "$built" "$work/plugin-$variant/kwin/plugins/screencast.so"
sha256sum "$work/plugin-$variant/kwin/plugins/screencast.so"
