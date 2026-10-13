#!/usr/bin/env python3
"""Record a real, isolated X11 desktop through the CLI, daemon, and native engine."""
import ctypes
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time


def memory_mib(pid, field):
    try:
        status = Path(f"/proc/{pid}/status").read_text()
    except FileNotFoundError:
        return None
    values = [int(line.split()[1]) / 1024 for line in status.splitlines() if line.startswith(f"{field}:")]
    return values[0] if values else None


def workers(daemon_pid):
    capture = []
    for process in Path("/proc").glob("[0-9]*"):
        try:
            parent = int((process / "stat").read_text().rsplit(")", 1)[1].split()[1])
            if parent == daemon_pid and (process / "comm").read_text().strip() == "wrec-capture":
                capture.append(int(process.name))
        except (FileNotFoundError, ProcessLookupError, IndexError, ValueError):
            pass
    return capture


def without_workers(daemon_pid):
    deadline = time.monotonic() + 5
    while workers(daemon_pid) and time.monotonic() < deadline:
        time.sleep(0.1)
    return not workers(daemon_pid)


def unmap(display, window):
    xlib = ctypes.CDLL("libX11.so.6")
    xlib.XOpenDisplay.restype = ctypes.c_void_p
    connection = xlib.XOpenDisplay(display.encode())
    assert connection, f"could not open {display}"
    xlib.XUnmapWindow(ctypes.c_void_p(connection), ctypes.c_ulong(window))
    xlib.XCloseDisplay(ctypes.c_void_p(connection))


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="wrec-x11-capture-") as directory:
        root = Path(directory)
        env = os.environ.copy()
        for key in ("WAYLAND_DISPLAY", "WREC_DAEMON_BIN", "WREC_CHANNEL", "WREC_HEADLESS"):
            env.pop(key, None)
        env.update(WREC_HOME=str(root / "home"), WREC_DATA_DIR=str(root / "data"))
        with open(root / "xvfb.log", "w") as log:
            screens = ["-screen", "0", "800x600x24", "-screen", "1", "3840x2160x24", "-nolisten", "tcp"]
            desktop = subprocess.Popen(["Xvfb", "-displayfd", "1", *screens], stdout=subprocess.PIPE, stderr=log, text=True)
            window = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(desktop.stdout, selectors.EVENT_READ)
                    assert selector.select(10), "Xvfb did not announce a display"
                env["DISPLAY"] = ":" + desktop.stdout.readline().strip()
                window = subprocess.Popen(["xmessage", "-title", "Wrec capture test", "-geometry", "500x250+50+50", "Wrec Linux capture: this is a real X11 test window."], env=env, stdout=log, stderr=log)

                def run(*args):
                    result = subprocess.run([str(binary), *args], env=env, capture_output=True, text=True, timeout=25)
                    assert result.returncode == 0, (args, result.stdout, result.stderr)
                    return json.loads(result.stdout)

                def job(job_id):
                    return run("job", "show", str(job_id), "--json")["job"]

                def wait(job_id, status):
                    deadline = time.monotonic() + 30
                    while time.monotonic() < deadline:
                        value = job(job_id)
                        if value["status"] == status:
                            return value
                        assert value["status"] not in ("failed", "cancelled"), value
                        time.sleep(0.1)
                    raise AssertionError(value)

                def submit(target, codec, duration=None):
                    args = ["record", "--target", target, "--codec", codec, "--no-system-audio", "--out", str(root / "movies"), "--detach", "--json"]
                    if duration:
                        args.extend(["--duration", duration])
                    return run(*args)["job"]["id"]

                def verify(value, codec):
                    path = value["output_path"]
                    assert Path(path).is_file(), value
                    probe = subprocess.run(["ffprobe", "-v", "error", "-show_streams", "-show_packets", "-of", "json", path], capture_output=True, text=True, check=True)
                    data = json.loads(probe.stdout)
                    assert data["streams"][0]["codec_name"] == codec, data["streams"]
                    previous = -float("inf")
                    for packet in data["packets"]:
                        dts = int(packet["dts"])
                        assert dts > previous, (previous, dts)
                        previous = dts
                    decoded = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-vsync", "0", "-enc_time_base", "-1", "-f", "null", "-"], capture_output=True, text=True)
                    assert decoded.returncode == 0 and not decoded.stderr, decoded.stderr
                    assert value["settings"]["hide_wrec"] is False
                    assert value["settings"]["show_mic_indicator"] is False
                    assert any(w["code"] == "linux_settings_unavailable" for w in value["warnings"]), value

                for _ in range(50):
                    targets = run("targets", "--json")
                    windows = [t for t in targets if t["kind"] == "window" and "Wrec capture test" in t["name"]]
                    if windows:
                        break
                    time.sleep(0.1)
                assert windows, targets
                print("PASS: X11 display and named-window discovery")

                display_id = submit("display:0", "h264", "2s")
                display = wait(display_id, "completed")
                verify(display, "h264")
                assert any("software encoding" in e["message"] for e in display["events"]), display
                print("PASS: real X11 display capture, automatic software fallback, H.264 decode, increasing timestamps")

                window_id = submit(f'window:{windows[0]["id"]}', "hevc")
                wait(window_id, "recording")
                time.sleep(0.5)
                run("job", "pause", str(window_id), "--json")
                wait(window_id, "paused")
                time.sleep(0.3)
                run("job", "resume", str(window_id), "--json")
                time.sleep(0.5)
                run("job", "pause", str(window_id), "--json")
                run("job", "stop", str(window_id), "--json")
                verify(wait(window_id, "completed"), "hevc")
                print("PASS: real X11 window capture, HEVC decode, pause/resume, and finalization while paused")

                daemon_pid = run("daemon", "status", "--json")["pid"]
                closing = subprocess.Popen(["xmessage", "-title", "Wrec closing window", "-geometry", "300x150+100+100", "Wrec Linux capture: this window closes mid-recording."], env=env, stdout=log, stderr=log)
                try:
                    for _ in range(50):
                        closing_windows = [target for target in run("targets", "--json") if target["kind"] == "window" and "Wrec closing window" in target["name"]]
                        if closing_windows:
                            break
                        time.sleep(0.1)
                    assert closing_windows, "closing window was not discovered"
                    closing_id = submit(f'window:{closing_windows[0]["id"]}', "h264")
                    wait(closing_id, "recording")
                    after_id = submit("display:0", "h264", "2s")
                    assert job(after_id)["status"] == "queued", job(after_id)
                    time.sleep(1)
                finally:
                    closing.terminate()
                    closing.wait(timeout=5)
                lost = wait(closing_id, "failed")
                assert any("X11 window closed" in event["message"] for event in lost["events"]), lost
                assert run("daemon", "status", "--json")["pid"] == daemon_pid, "daemon restarted after the captured window closed"
                verify(wait(after_id, "completed"), "h264")
                assert run("daemon", "status", "--json")["pid"] == daemon_pid, "daemon restarted during capture after source loss"
                print("PASS: closing a captured X11 window fails only that recording; the queued job records next in the same daemon")

                hidden = subprocess.Popen(["xmessage", "-title", "Wrec unmapped window", "-geometry", "300x150+120+120", "Wrec Linux capture: this window is unmapped mid-recording."], env=env, stdout=log, stderr=log)
                try:
                    for _ in range(50):
                        hidden_windows = [target for target in run("targets", "--json") if target["kind"] == "window" and "Wrec unmapped window" in target["name"]]
                        if hidden_windows:
                            break
                        time.sleep(0.1)
                    assert hidden_windows, "unmapped window was not discovered"
                    hidden_id = submit(f'window:{hidden_windows[0]["id"]}', "h264")
                    wait(hidden_id, "recording")
                    after_id = submit("display:0", "h264", "2s")
                    time.sleep(1)
                    unmapped_at = time.monotonic()
                    unmap(env["DISPLAY"], hidden_windows[0]["id"])
                    unmapped = wait(hidden_id, "failed")
                    assert time.monotonic() - unmapped_at < 5, "unmapping the captured window was not detected promptly"
                    assert any("was minimized" in event["message"] for event in unmapped["events"]), unmapped
                    verify(wait(after_id, "completed"), "h264")
                finally:
                    hidden.terminate()
                    hidden.wait(timeout=5)
                assert run("daemon", "status", "--json")["pid"] == daemon_pid, "daemon restarted after the captured window was unmapped"
                print("PASS: unmapping a captured X11 window fails only that recording; the queued job records next in the same daemon")

                held_id = submit("display:0", "h264")
                wait(held_id, "recording")
                held_workers = workers(daemon_pid)
                assert len(held_workers) == 1, held_workers
                time.sleep(1)
                os.kill(held_workers[0], signal.SIGSTOP)
                time.sleep(2)
                os.kill(held_workers[0], signal.SIGCONT)
                time.sleep(1)
                run("job", "stop", str(held_id), "--json")
                held = wait(held_id, "completed")
                verify(held, "h264")
                frozen_warnings = [w["message"] for w in held["warnings"] if w["code"] == "media_lost"]
                assert any("wrec did not run for 2." in message for message in frozen_warnings), held
                print("PASS: a capture worker stopped for 2 s finishes its movie and warns that the video may hold one picture there")

                frozen_id = submit("display:0", "h264")
                wait(frozen_id, "recording")
                frozen_workers = workers(daemon_pid)
                assert len(frozen_workers) == 1, frozen_workers
                queued_id = submit("display:0", "h264", "2s")
                assert job(queued_id)["status"] == "queued", job(queued_id)
                os.kill(frozen_workers[0], signal.SIGSTOP)
                run("job", "stop", str(frozen_id), "--json")
                deadline = time.monotonic() + 40
                while job(frozen_id)["status"] == "finishing" and time.monotonic() < deadline:
                    time.sleep(0.2)
                frozen = job(frozen_id)
                assert frozen["status"] == "failed", frozen
                assert any("did not exit within 20s" in event["message"] for event in frozen["events"]), frozen
                assert not Path(f"/proc/{frozen_workers[0]}").exists(), "frozen capture worker was not reaped"
                verify(wait(queued_id, "completed"), "h264")
                assert run("daemon", "status", "--json")["pid"] == daemon_pid, "daemon restarted after a frozen worker"
                print("PASS: a frozen capture worker is killed 20s after stop; the queued job records next in the same daemon")

                large_display = next(target for target in targets if target["kind"] == "display" and "3840×2160" in target["name"])
                worker_peak_mib = 0
                for codec in ("hevc", "h264", "hevc"):
                    memory_id = run("record", "--target", f'display:{large_display["id"]}', "--codec", codec, "--quality", "high", "--resolution", "native", "--no-system-audio", "--no-mic", "--duration", "2s", "--out", str(root / "movies"), "--detach", "--json")["job"]["id"]
                    deadline = time.monotonic() + 30
                    while job(memory_id)["status"] in ("queued", "starting", "recording", "finishing") and time.monotonic() < deadline:
                        for worker in workers(daemon_pid):
                            worker_peak_mib = max(worker_peak_mib, memory_mib(worker, "VmHWM") or 0)
                        time.sleep(0.2)
                    verify(wait(memory_id, "completed"), codec)
                    time.sleep(0.3)
                    idle_rss_mib = memory_mib(daemon_pid, "VmRSS")
                    assert idle_rss_mib < 256, f"4K encoder retained {idle_rss_mib:.1f} MiB after teardown"
                    assert without_workers(daemon_pid), "capture worker outlived its recording"
                assert worker_peak_mib > 0, "4K capture did not run in a wrec-capture worker"
                print(f"PASS: repeated 4K capture runs in a worker ({worker_peak_mib:.1f} MiB peak worker RSS) and leaves {idle_rss_mib:.1f} MiB daemon RSS")

                display_number = env["DISPLAY"][1:]
                for interrupted in ("recording", "paused"):
                    lost_id = submit("display:0", "h264")
                    wait(lost_id, "recording")
                    capture_workers = workers(daemon_pid)
                    assert len(capture_workers) == 1, capture_workers
                    queued_id = submit("display:0", "h264", "2s")
                    assert job(queued_id)["status"] == "queued", job(queued_id)
                    time.sleep(0.5)
                    if interrupted == "paused":
                        run("job", "pause", str(lost_id), "--json")
                        wait(lost_id, "paused")
                    desktop.kill()
                    desktop.wait(timeout=5)
                    lost = wait(lost_id, "failed")
                    assert any("capture worker exited unexpectedly" in event["message"] for event in lost["events"]), lost
                    queued = job(queued_id)
                    deadline = time.monotonic() + 30
                    while queued["status"] not in ("completed", "failed", "cancelled") and time.monotonic() < deadline:
                        time.sleep(0.1)
                        queued = job(queued_id)
                    assert queued["status"] == "failed", queued
                    assert run("daemon", "status", "--json")["pid"] == daemon_pid, "losing the X server restarted the daemon"
                    assert without_workers(daemon_pid), "crashed capture worker was not reaped"
                    desktop = subprocess.Popen(["Xvfb", f":{display_number}", *screens], stderr=log)
                    for _ in range(50):
                        if subprocess.run([str(binary), "targets", "--json"], env=env, capture_output=True).returncode == 0:
                            break
                        time.sleep(0.1)
                    after_id = submit("display:0", "h264", "2s")
                    verify(wait(after_id, "completed"), "h264")
                    assert run("daemon", "status", "--json")["pid"] == daemon_pid, "daemon restarted after X server loss"
                    print(f"PASS: losing the X server while {interrupted} fails that job and the queued job; the same daemon records after X returns")

                orphan_id = submit("display:0", "h264")
                wait(orphan_id, "recording")
                orphan_workers = workers(daemon_pid)
                assert len(orphan_workers) == 1, orphan_workers
                os.kill(daemon_pid, signal.SIGKILL)
                deadline = time.monotonic() + 5
                while Path(f"/proc/{orphan_workers[0]}").exists() and time.monotonic() < deadline:
                    time.sleep(0.1)
                assert not Path(f"/proc/{orphan_workers[0]}").exists(), "capture worker outlived a killed daemon"
                assert run("daemon", "start", "--json")["pid"] != daemon_pid, "a new daemon did not start after the old one was killed"
                print("PASS: killing the daemon kills its capture worker")
                run("daemon", "stop", "--json")
            finally:
                subprocess.run([str(binary), "daemon", "stop", "--json"], env=env, capture_output=True, timeout=20)
                if window:
                    window.terminate()
                    window.wait(timeout=5)
                desktop.terminate()
                desktop.wait(timeout=5)


if __name__ == "__main__":
    main()
