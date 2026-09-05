#!/usr/bin/env python3
"""Record a real, isolated X11 desktop through the CLI, daemon, and native engine."""
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time


def main():
    binary = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="wrec-x11-capture-") as directory:
        root = Path(directory)
        env = os.environ.copy()
        for key in ("WAYLAND_DISPLAY", "WREC_DAEMON_BIN", "WREC_CHANNEL", "WREC_HEADLESS"):
            env.pop(key, None)
        env.update(WREC_HOME=str(root / "home"), WREC_DATA_DIR=str(root / "data"))
        with open(root / "xvfb.log", "w") as log:
            desktop = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24", "-nolisten", "tcp"], stdout=subprocess.PIPE, stderr=log, text=True)
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
