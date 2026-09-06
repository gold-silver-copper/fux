#!/usr/bin/env python3
"""Frame cost of a fux binary per screen size and viewer count: bytes on the attachment socket
per keystroke, server CPU per keystroke and per output burst, wall latency, bytes per burst.

Usage: tools/measure_frames.py PATH/TO/fux [--keystrokes 100]
       [--config ROWSxCOLS[+ROWSxCOLS...] ...]

Each configuration attaches the listed viewers (raw attachment-protocol clients, all on the same
workspace and tab) to an isolated session server under a disposable HOME/XDG root. Every viewer
is drained concurrently so the server never sees a slow viewer. Reported per configuration:

- keystroke_bytes_median / _max: bytes received by the first viewer between sending one byte to
  the pane and the frame that shows its echo
- keystroke_latency_median_ms / _p95_ms: the wall time of the same exchange
- server_cpu_s_per_1000_keystrokes: server CPU seconds over the keystroke loop, scaled
- burst_s, burst_server_cpu_s, burst_bytes_first_viewer: a 20 000-line output burst
- rss_after_kib: server resident memory after the burst

Defaults: 24x80, 60x200, 24x80+24x80, 24x80 x4, 24x80 x8, 60x200+24x80. Nothing here touches
personal sessions.
"""
import argparse
import json
import os
import select
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DEFAULT_CONFIGS = ["24x80", "60x200", "24x80+24x80", "24x80+24x80+24x80+24x80",
                   "+".join(["24x80"] * 8), "60x200+24x80"]


def send(peer, value):
    data = json.dumps(value).encode()
    peer.sendall(struct.pack(">I", len(data)) + data)


class Viewer:
    """One raw attachment client with a non-blocking frame reader."""

    def __init__(self, path, rows, cols):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(5)
        self.sock.connect(str(path))
        send(self.sock, dict(type="hello", rows=rows, columns=cols))
        self.buffer = b""
        self.bytes = 0
        self.frames = []
        self.wait_frame(5)
        # The hello may arrive together with the bindings and the first frame.
        hello = self.frames[0]
        assert hello == {"hello": {}}, hello
        self.frames.clear()
        self.bytes = 0
        self.sock.setblocking(False)

    def pump(self):
        """Reads what is available; returns the number of complete frames appended."""
        appended = 0
        while True:
            try:
                data = self.sock.recv(1 << 20)
            except BlockingIOError:
                break
            except socket.timeout:
                break
            if not data:
                raise EOFError("server closed the attachment")
            self.buffer += data
            self.bytes += len(data)
            while len(self.buffer) >= 4:
                size = struct.unpack(">I", self.buffer[:4])[0]
                if len(self.buffer) < 4 + size:
                    break
                self.frames.append(json.loads(self.buffer[4:4 + size]))
                self.buffer = self.buffer[4 + size:]
                appended += 1
        return appended

    def wait_frame(self, timeout):
        end = time.monotonic() + timeout
        self.sock.setblocking(False)
        while time.monotonic() < end:
            ready, _, _ = select.select([self.sock], [], [], max(0, end - time.monotonic()))
            if ready and self.pump():
                return self.frames[-1]
        raise TimeoutError("no frame")


def frame_text(message):
    state = message.get("state", {}).get("state")
    if not state:
        return ""
    return "".join(
        cell.get("text", "") for pane in state.get("panes", {}).values() for cell in pane.get("cells", [])
    )


def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    total = 0.0
    for part in out.replace("-", ":").split(":"):
        total = total * 60 + float(part)
    return total


def rss_kib(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out or 0)


def drain_all(viewers, quiet):
    """Pumps every viewer until none received anything for `quiet` seconds."""
    last = time.monotonic()
    while time.monotonic() - last < quiet:
        ready, _, _ = select.select([v.sock for v in viewers], [], [], quiet)
        for viewer in viewers:
            if viewer.sock in ready and viewer.pump():
                last = time.monotonic()


def wait_text(viewers, first, predicate, timeout):
    """Pumps every viewer until a frame the first viewer received after the call started
    satisfies `predicate` (a delta carries only changed rows, so every new frame is checked)."""
    end = time.monotonic() + timeout
    seen = len(first.frames)
    while time.monotonic() < end:
        ready, _, _ = select.select([v.sock for v in viewers], [], [], max(0, end - time.monotonic()))
        for viewer in viewers:
            if viewer.sock in ready:
                viewer.pump()
        while seen < len(first.frames):
            if predicate(frame_text(first.frames[seen])):
                return
            seen += 1
    raise TimeoutError("frame did not arrive")


def measure(binary, config, keystrokes):
    sizes = [tuple(int(part) for part in item.split("x")) for item in config.split("+")]
    with tempfile.TemporaryDirectory(prefix="fux-frames-", dir="/tmp") as directory:
        root = Path(directory)
        env = os.environ.copy()
        env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=directory + "/config",
                   XDG_STATE_HOME=directory + "/state", SHELL="/bin/sh", TERM="xterm-256color")
        (root / "config/fux").mkdir(parents=True)
        (root / "config/fux/config.toml").write_text('default-command = { argv = ["/bin/sh"] }\n')
        server = subprocess.Popen([binary, "serve", "--name", "default"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        sock_path = root / "fux/default.attach.sock"
        viewers = []
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and not viewers:
                if sock_path.exists():
                    try:
                        viewers.append(Viewer(sock_path, *sizes[0]))
                    except (ConnectionRefusedError, FileNotFoundError):
                        time.sleep(0.005)
                else:
                    time.sleep(0.005)
                if server.poll() is not None:
                    raise RuntimeError(server.stderr.read().decode())
            assert viewers, "server did not start"
            for rows, cols in sizes[1:]:
                viewers.append(Viewer(sock_path, rows, cols))
            first = viewers[0]
            drain_all(viewers, 0.5)
            # Keystrokes: one byte each, echoed by the shell's line editor.
            latencies = []
            sizes_seen = []
            cpu_before = cpu_seconds(server.pid)
            for index in range(keystrokes):
                if index and index % 40 == 0:
                    send(first.sock, dict(type="input", bytes=[0x15]))  # clear the line
                    wait_text(viewers, first, lambda text: "a" not in text, 5)
                wanted = index % 40 + 1
                bytes_before = first.bytes
                begin = time.monotonic()
                send(first.sock, dict(type="input", bytes=[ord("a")]))
                wait_text(viewers, first, lambda text, wanted=wanted: text.count("a") >= wanted, 5)
                latencies.append(time.monotonic() - begin)
                sizes_seen.append(first.bytes - bytes_before)
            cpu_keys = cpu_seconds(server.pid) - cpu_before
            send(first.sock, dict(type="input", bytes=[0x15]))
            drain_all(viewers, 0.3)
            # Burst.
            bytes_before = first.bytes
            cpu_before = cpu_seconds(server.pid)
            send(first.sock, dict(type="input", bytes=list(
                b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURST''DONE\\\\n\n")))
            begin = time.monotonic()
            wait_text(viewers, first, lambda text: "BURSTDONE" in text, 60)
            burst = time.monotonic() - begin
            drain_all(viewers, 0.3)
            cpu_burst = cpu_seconds(server.pid) - cpu_before
            result = {
                "config": config,
                "keystroke_bytes_median": int(statistics.median(sizes_seen)),
                "keystroke_bytes_max": max(sizes_seen),
                "keystroke_latency_median_ms": round(statistics.median(latencies) * 1000, 2),
                "keystroke_latency_p95_ms": round(sorted(latencies)[int(len(latencies) * 0.95) - 1] * 1000, 2),
                "server_cpu_s_per_1000_keystrokes": round(cpu_keys * 1000 / keystrokes, 3),
                "burst_s": round(burst, 3),
                "burst_server_cpu_s": round(cpu_burst, 3),
                "burst_bytes_first_viewer": first.bytes - bytes_before,
                "rss_after_kib": rss_kib(server.pid),
            }
            for viewer in viewers:
                viewer.sock.close()
            return result
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary")
    parser.add_argument("--keystrokes", type=int, default=100)
    parser.add_argument("--config", action="append")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    results = [measure(binary, config, args.keystrokes) for config in args.config or DEFAULT_CONFIGS]
    print(json.dumps({"binary": binary, "results": results}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
