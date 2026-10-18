#!/usr/bin/env bash
# fetch.sh WORKDIR: download Ubuntu's kwin 4:6.6.6-0ubuntu0.1 source into
# WORKDIR, an existing directory you own, and check every file against
# SHA256SUMS. A file already there is kept if its checksum matches and
# reported otherwise; nothing is overwritten or deleted. Each download goes to
# a new temporary file that only this script removes, and a checked download
# is published with a hard link, which fails if the name appeared meanwhile.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=${1:?usage: fetch.sh WORKDIR}
[[ -d $work && -O $work ]] || { echo "fetch.sh: $work must be an existing directory you own" >&2; exit 1; }
base=https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/kwin/4:6.6.6-0ubuntu0.1
tmp=
trap '[[ -z $tmp ]] || rm -f -- "$tmp"' EXIT
for file in kwin_6.6.6.orig.tar.xz kwin_6.6.6-0ubuntu0.1.debian.tar.xz kwin_6.6.6-0ubuntu0.1.dsc; do
  want=$(awk -v f="$file" '$2 == f { print $1 }' "$here/SHA256SUMS")
  [[ -n $want ]]
  if [[ -e $work/$file ]]; then
    got=$(sha256sum "$work/$file" | cut -d' ' -f1)
    [[ $got == "$want" ]] || { echo "fetch.sh: $work/$file exists with sha256 $got, expected $want; move it away" >&2; exit 1; }
    echo "kept $file"
    continue
  fi
  tmp=$(mktemp "$work/.$file.XXXXXX")
  curl -fsSL --proto '=https' --max-time 300 -o "$tmp" "$base/$file"
  got=$(sha256sum "$tmp" | cut -d' ' -f1)
  [[ $got == "$want" ]] || { echo "fetch.sh: $file downloaded with sha256 $got, expected $want" >&2; exit 1; }
  ln -- "$tmp" "$work/$file" || { echo "fetch.sh: $work/$file appeared during the download; move it away" >&2; exit 1; }
  rm -f -- "$tmp"
  tmp=
  echo "fetched $file"
done
