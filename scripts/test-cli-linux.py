#!/usr/bin/env python3
"""Exercise an extracted Linux package without a desktop, GPU, or user config."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def main():
    prefix = Path(sys.argv[1]).resolve()
    binary = prefix / "bin" / "wrec"
    with tempfile.TemporaryDirectory(prefix="wrec-linux-smoke-") as directory:
        env = os.environ.copy()
        for key in ("WREC_DAEMON_BIN", "WREC_CAPTURE_ENGINE_PATH", "WREC_HEADLESS",
                    "WREC_CHANNEL", "WAYLAND_DISPLAY", "DISPLAY", "XDG_RUNTIME_DIR"):
            env.pop(key, None)
        env["WREC_HOME"] = str(Path(directory) / "home")
        env["WREC_DATA_DIR"] = str(Path(directory) / "data")

        def run(*args, success=True):
            result = subprocess.run([str(binary), *args], env=env, text=True,
                                    capture_output=True, timeout=20)
            assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
            return result

        # A sibling executable must not shadow the archive's own runtime.
        decoy = prefix / "bin" / "daemon"
        assert not decoy.exists(), decoy
        decoy.write_text("#!/bin/sh\nexit 99\n")
        decoy.chmod(0o755)
        pid = None
        try:
            status = json.loads(run("daemon", "start", "--json").stdout)
            assert status["channel"] == "release", status
            pid = status["pid"]
            executable = Path(f"/proc/{pid}/exe").resolve()
            assert executable == prefix / "lib" / "wrec" / "daemon", executable
            print("PASS: packaged CLI automatically launches its relative Linux daemon")

            result = run("targets", "--json", success=False)
            assert "Wayland or X11 desktop session" in result.stdout + result.stderr
            print("PASS: headless target discovery returns a useful error")

            result = run("record", "--target", "display:0", "--duration", "1s", "--json", success=False)
            assert "Wayland or X11 desktop session" in result.stdout + result.stderr
            assert not list(Path(directory).rglob("*.mov"))
            print("PASS: unsupported capture fails without a recording artifact")

            jobs = json.loads(run("jobs", "--json").stdout)
            assert jobs["jobs"] == [], jobs
            print("PASS: rejected requests leave no active or queued jobs")

            run("daemon", "stop", "--json")
            socket = Path(env["WREC_HOME"]) / "wrec.sock"
            for _ in range(100):
                if not socket.exists():
                    break
                time.sleep(0.05)
            assert not socket.exists(), "daemon socket survived shutdown"
            pid = None
            print("PASS: clean daemon shutdown removes the socket")

            status = json.loads(run("daemon", "start", "--json").stdout)
            pid = status["pid"]
            os.kill(pid, signal.SIGTERM)
            for _ in range(100):
                if not socket.exists():
                    break
                time.sleep(0.05)
            assert not socket.exists(), "daemon socket survived SIGTERM"
            pid = None
            print("PASS: restarted daemon handles SIGTERM and removes the socket")
        finally:
            decoy.unlink()
            if pid is not None:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    main()
