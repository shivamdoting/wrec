#!/usr/bin/env bash
# opt-out.sh: remove the drop-in opt-in.sh wrote, so KWin loads the distro's
# screencast plugin again from the next login. Leaves your build directory
# alone. Works whatever kwin version is installed now.
set -euo pipefail
unit=plasma-kwin_wayland.service
dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$unit.d
dropin=$dir/50-wrec-kwin-screencast.conf
marker='# wrec kwin-6.6.6 screencast backport'
if [[ ! -e $dropin ]]; then
  echo "opt-out.sh: $dropin is not there; nothing to undo"
  exit 0
fi
grep -qxF "$marker" "$dropin" || { echo "opt-out.sh: $dropin was not written by opt-in.sh; left alone" >&2; exit 1; }
rm -- "$dropin"
rmdir -- "$dir" 2> /dev/null || true
systemctl --user daemon-reload
echo "Removed $dropin. Log out and in to load the distro plugin again."
