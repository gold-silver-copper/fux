#!/usr/bin/env python3
"""Memory per history row of a fux binary: server RSS before and after a pane fills its history
with plain lines and with styled wide lines, per configured scrollback limit.

Usage: tools/measure_memory.py PATH/TO/fux [--scrollback 10000] [--rows 24] [--columns 80]

Reports RSS at start, after `scrollback` plain lines, and after `scrollback` styled lines of
wide characters in a second pane, with bytes per retained row for each. Nothing here touches
personal sessions.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from measure_frames import Viewer, drain_all, rss_kib, send, wait_text  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary")
    parser.add_argument("--scrollback", type=int, default=10000)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--columns", type=int, default=80)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="fux-memory-", dir="/tmp") as directory:
        root = Path(directory)
        env = os.environ.copy()
        env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=directory + "/config",
                   XDG_STATE_HOME=directory + "/state", SHELL="/bin/sh", TERM="xterm-256color")
        (root / "config/fux").mkdir(parents=True)
        (root / "config/fux/config.toml").write_text(
            f'default-command = {{ argv = ["/bin/sh"] }}\n[history]\nscrollback-lines = {args.scrollback}\n')
        server = subprocess.Popen([binary, "serve", "--name", "default"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        sock_path = root / "fux/default.attach.sock"
        try:
            deadline = time.monotonic() + 10
            viewer = None
            while time.monotonic() < deadline and viewer is None:
                if sock_path.exists():
                    try:
                        viewer = Viewer(sock_path, args.rows, args.columns)
                    except (ConnectionRefusedError, FileNotFoundError):
                        time.sleep(0.005)
                else:
                    time.sleep(0.005)
                if server.poll() is not None:
                    raise RuntimeError(server.stderr.read().decode())
            assert viewer is not None, "server did not start"
            viewers = [viewer]
            drain_all(viewers, 0.5)
            rss_start = rss_kib(server.pid)
            lines = args.scrollback + args.rows
            width = args.columns - 12
            send(viewer.sock, dict(type="input", bytes=list(
                f"i=0; while [ $i -lt {lines} ]; do printf '%0{width}d\\\\n' $i; i=$((i+1)); done; printf PLAIN''DONE\\\\n\n".encode())))
            wait_text(viewers, viewer, lambda text: "PLAINDONE" in text, 120)
            drain_all(viewers, 0.5)
            rss_plain = rss_kib(server.pid)
            # A second pane with styled wide characters (bold red CJK), same line count.
            send(viewer.sock, dict(type="control", request=dict(command="split", id=1, axis="horizontal")))
            wait_text(viewers, viewer, lambda text: True, 5)
            drain_all(viewers, 1.0)
            rss_split = rss_kib(server.pid)
            wide = "日" * (width // 4)
            send(viewer.sock, dict(type="input", bytes=list(
                f"i=0; while [ $i -lt {lines} ]; do printf '\\\\033[1;31m{wide}\\\\033[m%d\\\\n' $i; i=$((i+1)); done; printf WIDE''DONE\\\\n\n".encode())))
            wait_text(viewers, viewer, lambda text: "WIDEDONE" in text, 120)
            drain_all(viewers, 0.5)
            rss_wide = rss_kib(server.pid)
            print(json.dumps({
                "binary": binary,
                "scrollback": args.scrollback,
                "size": f"{args.rows}x{args.columns}",
                "rss_start_kib": rss_start,
                "rss_after_plain_kib": rss_plain,
                "bytes_per_plain_row": (rss_plain - rss_start) * 1024 // args.scrollback,
                "rss_after_split_kib": rss_split,
                "rss_after_wide_kib": rss_wide,
                "bytes_per_wide_row": (rss_wide - rss_split) * 1024 // args.scrollback,
            }, indent=2))
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    sys.exit(main())
