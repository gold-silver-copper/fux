#!/usr/bin/env python3
"""Viewer-side cost of a fux binary: the real `fux` viewer on a pty against an isolated server.

Usage: tools/measure_viewer.py PATH/TO/fux [--keystrokes 200] [--rows 24] [--columns 80]

Reports the viewer process's CPU seconds and the bytes it wrote to its terminal for a keystroke
loop (one echoed byte at a time) and for a 20 000-line output burst, plus the server's CPU for
the same phases. The pty is drained continuously so the viewer never blocks on a full buffer.
Nothing here touches personal sessions.
"""
import argparse
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

ESCAPES = re.compile(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]')


def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    total = 0.0
    for part in out.replace("-", ":").split(":"):
        total = total * 60 + float(part)
    return total


class Terminal:
    def __init__(self, binary, env, rows, columns):
        self.raw = b""
        self.read_bytes = 0
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
            os.execve(binary, [binary], env)

    def pump(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], max(0, end - time.monotonic()))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            self.raw += data
            self.raw = self.raw[-(1 << 20):]
            self.read_bytes += len(data)
        return True

    def text(self):
        return ESCAPES.sub(b"", self.raw)

    def wait_for(self, predicate, timeout):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump(0.02)
            if predicate(self.text()):
                return
        raise TimeoutError("viewer output did not arrive")

    def send(self, data):
        os.write(self.fd, data)

    def close(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except OSError:
            pass
        os.close(self.fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary")
    parser.add_argument("--keystrokes", type=int, default=200)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--columns", type=int, default=80)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="fux-viewer-", dir="/tmp") as directory:
        root = Path(directory)
        env = os.environ.copy()
        env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=directory + "/config",
                   XDG_STATE_HOME=directory + "/state", SHELL="/bin/sh", TERM="xterm-256color")
        (root / "config/fux").mkdir(parents=True)
        (root / "config/fux/config.toml").write_text('default-command = { argv = ["/bin/sh", "-c", "printf READY; exec /bin/sh"] }\n')
        server = subprocess.Popen([binary, "serve", "--name", "default"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        terminal = None
        try:
            sock_path = root / "fux/default.attach.sock"
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not sock_path.exists():
                time.sleep(0.005)
            terminal = Terminal(binary, env, args.rows, args.columns)
            terminal.wait_for(lambda text: b"READY" in text, 10)
            terminal.pump(0.5)
            # Keystrokes.
            viewer_cpu = cpu_seconds(terminal.pid)
            server_cpu = cpu_seconds(server.pid)
            bytes_before = terminal.read_bytes
            begin = time.monotonic()
            for index in range(args.keystrokes):
                if index and index % 40 == 0:
                    terminal.raw = b""
                    terminal.send(b"\x15")
                    terminal.wait_for(lambda text: b"a" not in text[-200:], 5)
                    terminal.raw = b""
                wanted = index % 40 + 1
                terminal.send(b"a")
                terminal.wait_for(lambda text, wanted=wanted: text.count(b"a") >= wanted, 5)
            keystrokes_s = time.monotonic() - begin
            viewer_cpu_keys = cpu_seconds(terminal.pid) - viewer_cpu
            server_cpu_keys = cpu_seconds(server.pid) - server_cpu
            bytes_keys = terminal.read_bytes - bytes_before
            terminal.send(b"\x15")
            terminal.pump(0.3)
            # Burst.
            terminal.raw = b""
            viewer_cpu = cpu_seconds(terminal.pid)
            server_cpu = cpu_seconds(server.pid)
            bytes_before = terminal.read_bytes
            terminal.send(b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURST''DONE\\\\n\n")
            begin = time.monotonic()
            terminal.wait_for(lambda text: b"BURSTDONE" in text, 60)
            burst_s = time.monotonic() - begin
            terminal.pump(0.3)
            print(json.dumps({
                "binary": binary,
                "size": f"{args.rows}x{args.columns}",
                "keystrokes": args.keystrokes,
                "keystroke_loop_s": round(keystrokes_s, 3),
                "viewer_cpu_s_per_1000_keystrokes": round(viewer_cpu_keys * 1000 / args.keystrokes, 3),
                "server_cpu_s_per_1000_keystrokes": round(server_cpu_keys * 1000 / args.keystrokes, 3),
                "pty_bytes_per_keystroke": bytes_keys // args.keystrokes,
                "burst_s": round(burst_s, 3),
                "burst_viewer_cpu_s": round(cpu_seconds(terminal.pid) - viewer_cpu, 3),
                "burst_server_cpu_s": round(cpu_seconds(server.pid) - server_cpu, 3),
                "burst_pty_bytes": terminal.read_bytes - bytes_before,
            }, indent=2))
            terminal.send(b"\x01d")
            terminal.pump(1)
        finally:
            if terminal is not None:
                terminal.close()
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    sys.exit(main())
