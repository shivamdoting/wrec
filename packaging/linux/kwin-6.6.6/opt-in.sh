#!/usr/bin/env bash
# opt-in.sh PLUGIN_ROOT: from your next login, have your KWin load
# PLUGIN_ROOT/kwin/plugins/screencast.so (from build.sh) instead of the
# distro's. Writes one systemd user drop-in that sets QT_PLUGIN_PATH for
# plasma-kwin_wayland.service, and nothing else: no system file, package or
# other user file changes. opt-out.sh removes it.
#
# Refuses unless kwin is exactly 4:6.6.6-0ubuntu0.1, the plugin and its
# directories are yours and writable by nobody else, and nothing else already
# sets QT_PLUGIN_PATH for KWin.
set -euo pipefail
root=${1:?usage: opt-in.sh PLUGIN_ROOT}
version=4:6.6.6-0ubuntu0.1
unit=plasma-kwin_wayland.service
dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$unit.d
dropin=$dir/50-wrec-kwin-screencast.conf
marker='# wrec kwin-6.6.6 screencast backport'
fail() { echo "opt-in.sh: $*" >&2; exit 1; }

for package in libkwin6 kwin-common kwin-wayland; do
  got=$(dpkg-query -W -f '${Version}' "$package" 2>/dev/null || true)
  [[ $got == "$version" ]] || fail "$package is '${got:-not installed}'; this plugin is built for $version only"
done
root=$(realpath -e "$root") || fail "$1 does not exist"
plugin=$root/kwin/plugins/screencast.so
[[ -f $plugin && ! -L $plugin ]] || fail "$plugin is not a regular file"
# KWin runs whatever this path holds, so nobody else may change it: the
# plugin and its directories are yours and writable only by you, and no
# directory above them lets anyone else rename them: none is writable by
# group or others unless it is sticky, like /tmp.
for path in "$plugin" "$root/kwin/plugins" "$root/kwin" "$root"; do
  owner=$(stat -c %u "$path")
  [[ $owner == "$(id -u)" || $owner == 0 ]] || fail "$path is owned by uid $owner"
  [[ $(( 0$(stat -c %a "$path") & 022 )) == 0 ]] || fail "$path is writable by group or others"
done
path=$root
while [[ $path != / ]]; do
  path=$(dirname "$path")
  owner=$(stat -c %u "$path")
  [[ $owner == "$(id -u)" || $owner == 0 ]] || fail "$path is owned by uid $owner"
  mode=0$(stat -c %a "$path")
  [[ $(( mode & 022 )) == 0 || $(( mode & 01000 )) != 0 ]] || fail "$path is writable by group or others"
done
[[ $root != *$'\n'* ]] || fail "the path contains a newline"
[[ $(systemctl --user show "$unit" -p LoadState --value) == loaded ]] || fail "$unit not found: this needs Plasma started by systemd"
if systemctl --user show-environment | grep -q '^QT_PLUGIN_PATH='; then
  fail "your user session already sets QT_PLUGIN_PATH; setting it for KWin would replace yours"
fi
for other in "$dir"/*.conf; do
  [[ -e $other && $other != "$dropin" ]] || continue
  if grep -q QT_PLUGIN_PATH "$other"; then fail "$other already sets QT_PLUGIN_PATH"; fi
done
if [[ -e $dropin ]]; then
  grep -qxF "$marker" "$dropin" || fail "$dropin exists and was not written by this script"
elif [[ $(systemctl --user show "$unit" -p Environment --value) == *QT_PLUGIN_PATH=* ]]; then
  fail "$unit already gets QT_PLUGIN_PATH from another drop-in"
fi

# systemd unquotes "..." with C escapes and expands % specifiers.
value=QT_PLUGIN_PATH=$root
value=${value//\\/\\\\}
value=${value//\"/\\\"}
value=${value//%/%%}
mkdir -p "$dir"
tmp=$(mktemp "$dir/.wrec-kwin-screencast.XXXXXX")
cat > "$tmp" <<EOF
$marker
# Written by opt-in.sh for kwin $version, plugin sha256
# $(sha256sum "$plugin" | cut -d' ' -f1).
# A KWin upgrade leaves this plugin built for the old KWin. Run opt-out.sh
# before upgrading kwin, or right after, and log out and in.
[Service]
Environment="$value"
EOF
mv -f "$tmp" "$dropin"
systemctl --user daemon-reload
[[ $(systemctl --user show "$unit" -p Environment --value) == *"QT_PLUGIN_PATH=$root"* ]] \
  || fail "systemd did not take $dropin; remove it with opt-out.sh"
echo "Wrote $dropin. Log out and in, then check with:"
echo "  sudo grep -F screencast.so /proc/\$(pgrep -u \"\$USER\" -x kwin_wayland)/maps"
echo "which should show $plugin."
