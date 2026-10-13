# KWin 6.6.6 screencast timestamp backport (experimental, opt-in)

KWin 6.6 and 6.7 stamp each screen-capture frame with the time of the
screen's previous page flip, not the time the frame was captured. After the
screen sits still, that stamp is as old as the still period, so in a wrec
recording on KDE Plasma a change that follows a short pause shows up early by
up to about a second, short flashes last too long, and the first change after
`wrec job resume` can be missing. wrec trusts the compositor's stamp, so the fix
belongs in KWin.

KDE fixed it upstream in commit
[cf5049251db3](https://invent.kde.org/plasma/kwin/-/commit/cf5049251db3654d317d04631b65b827ef4099ca)
("screencast: Timestamp frames at capture time", David Weber, 2026-08-08),
which samples the time once per frame just before rendering it. It is in the
Plasma 6.8 betas (v6.7.90, v6.7.91) and not in any 6.6.x or 6.7.x release.

This directory backports that commit to Ubuntu's KWin 6.6.6 and builds only the
screencast plugin, which you load for your own user. Nothing here is installed
by wrec, and the wrec package works without it.

## What was tested

Only this stack, in a 2-vCPU VM with software rendering:

| | |
|---|---|
| Distribution | Ubuntu 26.04 (resolute), amd64 |
| KWin packages | `kwin-wayland`, `kwin-common`, `libkwin6`, `kwin-dev` 4:6.6.6-0ubuntu0.1 |
| Toolchain | g++ 15.2.0, cmake 4.2.3, extra-cmake-modules 6.24.0, Qt 6.10.2, KF6 6.24.0, PipeWire 1.6.2 |
| Result | flashes after short pauses placed −7 to +22 ms from when the app drew them (stock: about −425 ms); flash length 140 to 190 ms for 150 ms drawn (stock: about 450 ms); first change after a resume kept 9 of 9 times (stock: 0 of 12); no paused content leaked |

Stock KDE is not a supported wrec setup: these results come from the patched
plugin. Plasma 6.8 contains the upstream fix but also moves recording to
vsync-paced frames, and has not been tested with wrec. Other distributions,
KWin builds, real GPUs and X11 sessions are untested.

## Files

| file | what it is |
|---|---|
| `kwin-6.6.6-screencast-capture-time-pts.diff` | cf5049251db3 applied to Ubuntu's kwin 6.6.6 plugin sources (offsets and one fuzz when ported, no hand edits). It removes `clock()` from the screencast sources and samples `steady_clock::now()` in `ScreenCastStream::record()` before rendering. Output, window and region casts, memfd and DMA-BUF buffers. |
| `CMakeLists.txt` | builds only `src/plugins/screencast` against the installed libkwin6 and kwin-dev, with KWin 6.6.6's compile settings |
| `fetch.sh WORKDIR` | downloads Ubuntu's source for kwin 4:6.6.6-0ubuntu0.1 from Launchpad and checks `SHA256SUMS` |
| `build.sh WORKDIR [patched\|stock]` | checks the installed KWin version and the sources, applies Ubuntu's patches and the backport, builds the plugin |
| `opt-in.sh PLUGIN_ROOT` / `opt-out.sh` | add or remove one systemd user drop-in that points KWin at the built plugin |
| `SHA256SUMS` | the three source files and the diff |

`kwin_6.6.6.orig.tar.xz` is KDE's own `kwin-6.6.6.tar.xz`: both have sha256
`76314bb5…f561953`. The plugin directory is the same as upstream tag v6.6.6;
Ubuntu's two patches touch only `CMakeLists.txt` and `src/core/drmdevice.cpp`.

## Build

Install the build dependencies (on the tested system this added 114 packages
and changed none):

```bash
sudo apt install --no-install-recommends cmake g++ extra-cmake-modules kwin-dev \
  libkf6i18n-dev libpipewire-0.3-dev libdrm-dev pkgconf make
```

Then, in a new directory of your choice:

```bash
mkdir ~/kwin-screencast-backport
packaging/linux/kwin-6.6.6/fetch.sh ~/kwin-screencast-backport
packaging/linux/kwin-6.6.6/build.sh ~/kwin-screencast-backport
```

`build.sh` refuses to run unless `libkwin6`, `kwin-dev`, `kwin-common` and
`kwin-wayland` are all 4:6.6.6-0ubuntu0.1. It writes only `src-patched/`,
`cmake-patched/`, `build-patched/` and `plugin-patched/` in that directory,
fails if any of them already exists, and never deletes anything. It took 45
seconds at two jobs in the test VM. `build.sh DIR stock` builds the
unpatched source the same way, for comparison.

## Use

```bash
packaging/linux/kwin-6.6.6/opt-in.sh ~/kwin-screencast-backport/plugin-patched
```

then log out and back in. `opt-in.sh` writes
`~/.config/systemd/user/plasma-kwin_wayland.service.d/50-wrec-kwin-screencast.conf`,
which sets `QT_PLUGIN_PATH` for KWin only. KWin then finds the backported
`screencast.so` before the distro's; no system file changes. It refuses if:

- any of `libkwin6`, `kwin-common`, `kwin-wayland` is not 4:6.6.6-0ubuntu0.1,
- the plugin or one of its directories belongs to another user or is writable
  by group or others, or a directory above them is writable by group or others
  without the sticky bit (KWin would run whatever is there),
- your session or another drop-in already sets `QT_PLUGIN_PATH`, or a file of
  the same name exists that it did not write,
- Plasma is not started through systemd (`plasma-kwin_wayland.service` missing).

To check that KWin loaded it after logging in:

```bash
sudo grep -F screencast.so /proc/$(pgrep -u "$USER" -x kwin_wayland)/maps
```

(KWin's memory map needs sudo to read.)

## Undo

```bash
packaging/linux/kwin-6.6.6/opt-out.sh
```

then log out and in. KWin loads `/usr/lib/x86_64-linux-gnu/qt6/plugins/kwin/plugins/screencast.so`
again. `opt-out.sh` only removes a drop-in that `opt-in.sh` wrote. Delete the
work directory yourself, and remove the build dependencies with apt if you
installed them only for this.

## KWin upgrades

The plugin links against the exact libkwin it was built with. The version check
runs only when you opt in. If apt later upgrades KWin, your session keeps
loading the old plugin, which can fail to load (no screen recording) or crash
KWin at login. Before upgrading kwin, run `opt-out.sh`. If you upgraded first
and the session no longer starts, switch to a text console (Ctrl+Alt+F3), log
in, and run:

```bash
rm ~/.config/systemd/user/plasma-kwin_wayland.service.d/50-wrec-kwin-screencast.conf
systemctl --user daemon-reload
```

then log in again. Rebuild for a new KWin only if this directory gains a
backport for that version.

## License

The diff changes KWin source files and is under their licenses:
`screencaststream.cpp` and `.h` are LGPL-2.0-or-later, the other seven files
GPL-2.0-or-later (see their SPDX headers and KWin's `LICENSES/`). The commit
it ports is by David Weber for KDE. `CMakeLists.txt` copies compile settings
from KWin 6.6.6's build files. The scripts are part of wrec (MIT).
