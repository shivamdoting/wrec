#!/usr/bin/env bash
# build.sh TAG OUT [patched|stock]: build xdg-desktop-portal-wlr TAG (v0.7.1 or
# v0.8.1) from upstream with wrec's patch series for that tag (`patched`, the
# default), or unchanged (`stock`) as a control.
#
# OUT must not exist. build.sh creates it and writes only inside it: src/ (the
# upstream source), patches/ (checked copies of the series, when patched),
# build/, the logs, and the binary OUT/xdg-desktop-portal-wlr. It never deletes anything; after
# a failure, read the log it names and remove OUT yourself. Nothing is
# installed. Runs at nice 19 with JOBS (default 2) compile jobs.
set -euo pipefail
export LC_ALL=C
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
usage='usage: build.sh v0.7.1|v0.8.1 OUT [patched|stock]'
fail() { echo "build.sh: $*" >&2; exit 1; }
[[ $# == 2 || $# == 3 ]] || { echo "build.sh: $usage" >&2; exit 2; }
tag=$1 out=$2 variant=${3:-patched}
# The commit each tag must point to, and the source tree the patched build
# must end with: the tree that was tested (see README.md).
case $tag in
  v0.7.1) commit=74428f2a8fa7f252e2a46fdf5b697536c66c8a1c patched_tree=fa91cc73dfd3a613b578b5f679d674d698fc9590 ;;
  v0.8.1) commit=e1d5d16f0e064a0b788c5d70c11461b15ee7a4af patched_tree=d88ae1813a64261957cb6ab3e1958a7259d69632 ;;
  *) echo "build.sh: unknown tag '$tag'; $usage" >&2; exit 2 ;;
esac
[[ $variant == patched || $variant == stock ]] || { echo "build.sh: unknown variant '$variant'; $usage" >&2; exit 2; }
[[ -n $out ]] || { echo "build.sh: OUT is empty; $usage" >&2; exit 2; }
jobs=${JOBS:-2}
[[ $jobs =~ ^[1-9][0-9]*$ ]] || fail "JOBS must be a positive number, not '$jobs'"
for tool in git meson ninja pkg-config cc sha256sum; do
  command -v "$tool" > /dev/null || fail "$tool is not installed; see README.md"
done
if [[ $tag == v0.8.1 ]] && ! pkg-config --atleast-version=1.37 wayland-protocols; then
  fail "v0.8.1 needs wayland-protocols 1.37 or newer, found '$(pkg-config --modversion wayland-protocols 2> /dev/null || echo none)'; see README.md"
fi

# The series is the SHA256SUMS lines for this tag, in file order, and nothing else.
series=$(awk -v d="patches/$tag/" 'index($2, d) == 1 { print $2 }' "$here/SHA256SUMS")
[[ -n $series ]] || fail "SHA256SUMS lists no patches for $tag"
[[ $(cd "$here" && printf '%s\n' patches/"$tag"/*) == "$(sort <<< "$series")" ]] \
  || fail "patches/$tag/ holds files that SHA256SUMS does not list, or misses some"
(cd "$here" && grep -F " patches/$tag/" SHA256SUMS | sha256sum -c --quiet -) \
  || fail "a patch in patches/$tag/ does not match SHA256SUMS"

mkdir -- "$out" || fail "could not create $out; OUT must be a new path"
out=$(realpath -e -- "$out")
applied=()
if [[ $variant == patched ]]; then
  mkdir "$out/patches"
  while read -r path; do
    want=$(awk -v p="$path" '$2 == p { print $1 }' "$here/SHA256SUMS")
    copy=$out/patches/$(basename "$path")
    cp -- "$here/$path" "$copy"
    got=$(sha256sum "$copy" | cut -d' ' -f1)
    [[ $got == "$want" ]] || fail "$path changed while copying: sha256 $got, expected $want"
    applied+=("$copy")
  done <<< "$series"
fi

# Fetch only the tag, then check it still names the pinned commit.
git init -q "$out/src"
git -C "$out/src" fetch -q --depth 1 https://github.com/emersion/xdg-desktop-portal-wlr.git "refs/tags/$tag:refs/tags/$tag" \
  || fail "could not fetch $tag from upstream"
got=$(git -C "$out/src" rev-parse "$tag^{commit}")
[[ $got == "$commit" ]] || fail "upstream $tag is $got, expected $commit"
git -C "$out/src" -c advice.detachedHead=false checkout -q --detach "$commit"
if [[ $variant == patched ]]; then
  # No hooks and no whitespace fixes from your git config; the tree check
  # below catches any other change.
  git -C "$out/src" -c core.hooksPath=/dev/null -c user.name=wrec -c user.email=wrec@localhost \
    am -q --no-3way --whitespace=nowarn "${applied[@]}" > "$out/am.log" 2>&1 \
    || fail "a patch did not apply to $tag, see $out/am.log"
  got=$(git -C "$out/src" rev-parse 'HEAD^{tree}')
  [[ $got == "$patched_tree" ]] || fail "patched source tree is $got, expected $patched_tree"
fi

# Same configuration as the tested builds: meson's default debug build type,
# /usr prefix (so the system-wide config is read from /etc/xdg, as with distro
# builds), no man pages. Older C headers need unistd.h spelled out for v0.8.1.
args=(--prefix=/usr -Dman-pages=disabled)
[[ $tag == v0.8.1 ]] && args+=("-Dc_args=-include unistd.h")
nice -n19 meson setup "$out/build" "$out/src" "${args[@]}" > "$out/setup.log" 2>&1 \
  || fail "meson setup failed, see $out/setup.log"
nice -n19 ninja -C "$out/build" -j "$jobs" > "$out/build.log" 2>&1 \
  || fail "build failed, see $out/build.log"
install -m 0755 "$out/build/xdg-desktop-portal-wlr" "$out/xdg-desktop-portal-wlr"
echo "$tag $variant: source tree $(git -C "$out/src" rev-parse 'HEAD^{tree}')"
sha256sum "$out/xdg-desktop-portal-wlr"
