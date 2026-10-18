# xdg-desktop-portal-wlr screencast patches (experimental, opt-in)

On wlroots desktops such as Sway, wrec records the screen through
xdg-desktop-portal-wlr (xdpw). Stock xdpw has bugs that break recordings:

- 0.8.1 on the ext-image-copy path exits with the Wayland error "session
  already has a frame object" when a consumer pauses its stream during a
  capture and then resumes. GStreamer's `pipewiresrc`, which wrec uses, pauses
  once while starting, so recordings can fail at startup. On the
  wlr-screencopy path the same pause makes 0.8.1 crash.
- On a screen that does not change, a new stream (or a stream whose PipeWire
  buffers were just reallocated) can wait forever for its first frame, and
  0.8.1 can hand out an all-zero buffer that looks like a black frame.
- 0.7.1 gives the second and later recordings on a still screen no first
  frame, leaks about 21 file descriptors per closed session, and burns CPU
  (18 s of CPU time in 15 s) while the consumer holds every buffer.

This directory holds wrec's patch series for two upstream releases, v0.7.1
and v0.8.1, and a script that builds either one from pinned upstream source.
Nothing here is installed, selected or loaded by wrec, and the wrec package
does not include it. With stock xdpw, wrec notices when the portal exits and
fails the job; it does not repair the portal.

## Upstream status

Checked on 2026-10-10: the newest release is v0.8.4 and master is c0255d7
(2026-08-13), both unchanged since a comparison on 2026-10-03. In that
comparison v0.8.4 and master still exited after a pause and resume (3 of 3
runs each) and sent no first frame on a still screen. The open upstream
[PR 340](https://github.com/emersion/xdg-desktop-portal-wlr/pull/340) ("Do
nothing if capture frame is already pending", e99c205) avoided the exit, but
its first frame after a resume was all zeros in 5 of 5 runs. These patches
have not been sent upstream.

## Pins

| series | upstream tag | commit | patched source tree |
|---|---|---|---|
| legacy | v0.7.1 | 74428f2a8fa7f252e2a46fdf5b697536c66c8a1c | fa91cc73dfd3a613b578b5f679d674d698fc9590 |
| modern | v0.8.1 | e1d5d16f0e064a0b788c5d70c11461b15ee7a4af | d88ae1813a64261957cb6ab3e1958a7259d69632 |

`build.sh` refuses to build if the upstream tag names another commit, if a
patch does not match `SHA256SUMS`, or if the patched source is not exactly
this tree. The tree id covers every file, so a matching tree is the tested
source byte for byte. How the trees were tied to the tested builds:

- v0.7.1: applying the three patches with their original committer and
  dates reproduces the tested commit a9155888c1a45a08fda1e2018177511d61790130.
  Rebuilt on Debian 12 with the same flags and directory layout as the tested
  binary, the result matched it in every section except the build directory
  in the debug info and the build-id note.
- v0.8.1: the patches do not carry the original committer and dates, so the
  tested commit id (bffd83b) cannot be reproduced. After each patch, every file it
  touches matches the abbreviated blob id in the patch's `index` line (14 file
  states). The 2026-10-10 wrec run below used a binary built from bffd83b with
  sha256 8ac22a934b4d6b9eea55461f8aef1e55c6a9ae1968aa94e838698686c01e2677. A
  rebuild gives a different binary hash, because the compiler and build path
  end up in the binary.

## Patches

`patches/v0.7.1/`, applied in order (tested as a whole):

1. `0001` copies the first frame without waiting for damage. 0.7.1 asks for
   every frame with `copy_with_damage`, which only completes once the output
   changes. A new stream, or one with new PipeWire buffers, now gets a plain
   copy.
2. `0002` destroys a closed stream without waiting for its pending frame.
   Before, the stream was only marked to quit, and on a still screen that
   frame never finished, so each closed session leaked its stream, buffers
   and about 21 fds. Upstream fixed the same thing in f709e98 for 0.8.0.
3. `0003` waits one frame interval when the consumer holds every buffer,
   instead of asking for the next frame at once (18 s of CPU in 15 s), and
   cancels a pending frame when the stream pauses.

`patches/v0.8.1/`, applied in order:

1. `0001` cancels the pending capture when its buffer goes back to PipeWire
   (stream pause, `remove_buffer`, or an armed frame-limiter timer). Before,
   the next capture broke the ext-image-copy protocol and xdpw exited, and on
   wlr-screencopy the old frame's damage event dereferenced a NULL buffer.
2. `0002` gets a first frame without waiting for damage. wlr-screencopy gets
   the plain copy from v0.7.1's 0001. On ext-image-copy, wlroots captures only
   on damage after a session's first frame, so a new capture session is
   started when the consumer has no image, and identical constraints from
   that session no longer trigger a renegotiation.
3. `0003` sends buffers without a complete image as empty. Stock 0.8.1 set
   `spa_chunk.size` to the full buffer size when creating a buffer, so after a
   renegotiation the consumer could get a never-filled buffer that looked like
   a valid black frame, and a half-captured buffer returned on pause went out
   nonempty with timestamp 0. New buffers now start empty and marked
   corrupted, and only complete frames get their real size.
4. `0004` starts a new capture session after cancelling a frame the
   compositor had already copied. wlroots had cleared that session's damage,
   so the next capture waited forever on a still screen. `pipewiresrc` pauses
   about 12 ms after its link goes active, so this race hit most recordings:
   with wrec on a still output, 14 of 25 recordings stalled with 0001 to 0003,
   and 25 of 25 recorded with 0004 (the race hit in 21).

## What was tested

All tests ran xdpw directly in private, isolated sessions (headless Sway,
private PipeWire, WirePlumber and D-Bus), with a small C PipeWire client,
GStreamer, or wrec as the consumer. No logged-in desktop used these builds.

| series | where | result |
|---|---|---|
| v0.7.1 patched | Debian 12, Sway 1.7, wlroots 0.15, PipeWire 0.3.65 (2026-10-03) | 40 of 40 still-screen and 20 of 20 moving first frames; fds stayed at 19 across sessions; 0 CPU ticks idle. Stock: no first frame for sessions 2 and later on a still screen, about 21 fds leaked per closed session |
| v0.8.1 0001 to 0002, ext-image-copy | Ubuntu 26.04, Sway 1.11, wlroots 0.19.2, PipeWire 1.6.2 (2026-10-03) | survived pause and resume 3 of 3 (stock exited 5 of 5); first frame on a still screen after renegotiation (3 of 3) and after a 6 s pause (2 of 2); a 12-scenario suite passed with idle CPU at 0 and flat fds |
| v0.8.1 0001 to 0003, same fixture | same | no all-zero frames and no stalls in 28 GStreamer 1.28 sessions (stock and 0001 to 0002: all-zero frames in 3 of 4 C-client sessions) |
| v0.8.1 0001 to 0004, same fixture | same | 25 of 25 wrec recordings on a still output |
| v0.8.1 patched, wlr-screencopy | Debian 12, Sway 1.7 (2026-10-03; which patches were applied at that point was not recorded) | pause, still-screen repeat and a 40-session soak passed; stock crashed on pause and resume |
| v0.8.1 0001 to 0004, with wrec | Ubuntu 26.04, AMD Cezanne GPU, Sway 1.11, PipeWire 1.6.2, wrec 85100c3 (2026-10-10) | 21 of 21 recording cases passed; all 16 recordings used shared GPU buffers and VA encoding on the first try; killing xdpw failed the job 0.2 s later, and a restarted xdpw recorded again. That fixture captured about 23 fps, so 60 fps is not claimed |

Not tested: stock xdpw with wrec beyond the failures above, other wlroots
compositors (Hyprland, river, labwc and others), other GPUs, distributions
other than Debian 12 and Ubuntu 26.04, and the systemd opt-in below on a real
login session. Its drop-in was only checked with `systemd-analyze verify`
against Debian 12's unit.

## Files

| file | what it is |
|---|---|
| `patches/v0.7.1/*.patch`, `patches/v0.8.1/*.patch` | the two series, as `git format-patch` wrote them |
| `SHA256SUMS` | the patches, in the order they apply |
| `build.sh TAG OUT [patched\|stock]` | builds one series from upstream; see below |
| `LICENSE.xdg-desktop-portal-wlr` | upstream's license, which covers the patches |

## Build

Pick the series that matches your distribution's xdpw: v0.7.1 for 0.7.x (for
example Debian 12's 0.7.0), v0.8.1 for 0.8.x. v0.8.1 also passed the
wlr-screencopy tests above on Debian 12's Sway 1.7. Other pairings are
untested.
Install the build dependencies (Debian and Ubuntu names):

```bash
sudo apt install --no-install-recommends git meson ninja-build pkgconf gcc \
  libwayland-dev wayland-protocols libpipewire-0.3-dev libinih-dev \
  libgbm-dev libdrm-dev libsystemd-dev
```

Then build into a path that does not exist yet:

```bash
packaging/linux/xdg-desktop-portal-wlr/build.sh v0.8.1 ~/xdpw-wrec-v0.8.1
```

`build.sh` creates that directory and writes only inside it: the upstream
source in `src/`, checked copies of the patches, the meson build, logs, and
the binary `xdg-desktop-portal-wlr` at the top. It refuses an existing path,
including a symlink, and never deletes anything; after a failure, read the
log it names and remove the directory yourself. It fetches only the pinned
tag over HTTPS, applies the patches with `git am`, and builds with meson's
default debug build type and `--prefix=/usr` (so the system-wide config is
read from `/etc/xdg`, as with distribution builds) at nice 19 with two jobs
(set `JOBS` to change that). Each build took under 10 seconds on a 16-core
machine. `build.sh TAG OUT stock` builds the unpatched release the same way,
for comparison.

v0.8.1 needs wayland-protocols 1.37 or newer. Debian 12 has 1.31. The
protocols are XML files read only at build time, so you can install a newer
copy into your home directory and point the build at it:

```bash
git clone --depth 1 --branch 1.47 https://gitlab.freedesktop.org/wayland/wayland-protocols.git ~/wayland-protocols-1.47-src
git -C ~/wayland-protocols-1.47-src rev-parse HEAD   # 88223018d1b578d0d8869866da66d9608e05f928
meson setup ~/wayland-protocols-1.47-build ~/wayland-protocols-1.47-src --prefix="$HOME/wayland-protocols-1.47" -Dtests=false
meson install -C ~/wayland-protocols-1.47-build
PKG_CONFIG_PATH=$HOME/wayland-protocols-1.47/share/pkgconfig \
  packaging/linux/xdg-desktop-portal-wlr/build.sh v0.8.1 ~/xdpw-wrec-v0.8.1
```

Debian 12's wayland-scanner prints "validity error" lines about newer XML
attributes while building against 1.47; the build still succeeds. `build.sh`
also passes `-include unistd.h` for v0.8.1, because upstream calls `close()`
without including it and Debian 12's headers do not pull it in.

## Use (manual)

This replaces the portal for your user, so every app's screen sharing then
goes through the patched build. It needs your distribution's xdpw package
installed and started as the systemd user service `xdg-desktop-portal-wlr`.
Check that, and that nothing else already overrides it:

```bash
systemctl --user cat xdg-desktop-portal-wlr.service
```

Copy the binary and add a drop-in that points the service at it. This
subshell stops if either destination already exists or any setup step fails.
Run it only after confirming there are no other service overrides:

```bash
(
  set -eu
  portal_binary="$HOME/.local/libexec/xdg-desktop-portal-wlr-wrec"
  portal_dropin="$HOME/.config/systemd/user/xdg-desktop-portal-wlr.service.d/wrec.conf"
  for portal_path in "$portal_binary" "$portal_dropin"; do
    if [ -e "$portal_path" ] || [ -L "$portal_path" ]; then
      printf 'Refusing existing path: %s\n' "$portal_path" >&2
      exit 1
    fi
  done
  mkdir -p "$(dirname "$portal_binary")" "$(dirname "$portal_dropin")"
  (set -C; cat "$HOME/xdpw-wrec-v0.8.1/xdg-desktop-portal-wlr" > "$portal_binary")
  chmod 0755 "$portal_binary"
  (set -C; printf '[Service]\nExecStart=\nExecStart=%%h/.local/libexec/xdg-desktop-portal-wlr-wrec\n' > "$portal_dropin")
  systemctl --user daemon-reload
  systemctl --user restart xdg-desktop-portal-wlr.service
)
```

Restarting the portal ends any screen sharing in progress. Check which binary
runs:

```bash
readlink /proc/$(systemctl --user show -p MainPID --value xdg-desktop-portal-wlr.service)/exe
```

The binary is linked against the libraries it was built with. If a system
upgrade changes them and the portal stops starting, screen sharing stops for
every app until you undo this or rebuild. Upgrading the distribution's xdpw
package does not replace the patched binary.

## Undo

Remove only the files created by the setup above. If it refused an existing
path, leave that file alone. Save the service contents for inspection before
removing the drop-in. These commands stop if removal or service restoration
fails:

```bash
(
set -eu
rm ~/.config/systemd/user/xdg-desktop-portal-wlr.service.d/wrec.conf
rmdir ~/.config/systemd/user/xdg-desktop-portal-wlr.service.d 2> /dev/null || true
systemctl --user daemon-reload
systemctl --user restart xdg-desktop-portal-wlr.service
rm ~/.local/libexec/xdg-desktop-portal-wlr-wrec
)
```

Then delete the build directory, any wayland-protocols copy you made, and the
build dependencies if you installed them only for this.

## License

xdg-desktop-portal-wlr is MIT licensed, copyright 2018 emersion; its license
text is in `LICENSE.xdg-desktop-portal-wlr`. The patches retain their author attribution and modify that MIT-licensed
source. wrec's contributions, including `build.sh`, use the repository's
[MIT license](../../../LICENSE).
