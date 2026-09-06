#!/usr/bin/env python3
"""Reproducible local measurements for a fux binary: startup, idle CPU, memory, latency.

Usage: tools/measure.py PATH/TO/fux [--version 2|3] [--samples N]

Runs an isolated session server under a disposable HOME/XDG root, attaches one raw
attachment-protocol client and reports:

- startup: time from `fux serve` spawn until the workspace attachment socket accepts a hello
- idle-cpu: server CPU seconds consumed during a 10 s idle period with one attached viewer
- idle-wakeups: voluntary context switches during that idle period (Linux only; "n/a" elsewhere)
- rss: server resident memory after startup and after a 20000-line output burst
- latency: median/p95 wall time from sending `printf MARK` input until a state frame shows it

The attachment protocol version defaults to the one the binary speaks (3 for the ECS rewrite,
2 for the pre-rewrite baseline). Nothing here touches personal sessions.
"""
import argparse
import json
import os
import re
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def send(peer, value):
    data = json.dumps(value).encode()
    peer.sendall(struct.pack(">I", len(data)) + data)


def exact(peer, size):
    data = b""
    while len(data) < size:
        chunk = peer.recv(size - len(data))
        if not chunk:
            raise EOFError("server closed")
        data += chunk
    return data


def receive(peer):
    return json.loads(exact(peer, struct.unpack(">I", exact(peer, 4))[0]))


def frame_text(message):
    state = message.get("state", {}).get("state")
    if not state:
        return ""
    return "".join(
        cell.get("text", "") for pane in state.get("panes", {}).values() for cell in pane.get("cells", [])
    )


def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    parts = out.replace("-", ":").split(":")
    total = 0.0
    for part in parts:
        total = total * 60 + float(part)
    return total


def rss_kib(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return int(out or 0)


def voluntary_switches(pid):
    path = Path(f"/proc/{pid}/status")
    if not path.exists():
        return None
    match = re.search(r"voluntary_ctxt_switches:\s+(\d+)", path.read_text())
    return int(match.group(1)) if match else None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary")
    parser.add_argument("--version", type=int, default=3)
    parser.add_argument("--samples", type=int, default=20)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="fux-measure-", dir="/tmp") as directory:
        root = Path(directory)
        env = os.environ.copy()
        env.update(HOME=directory, XDG_RUNTIME_DIR=directory, XDG_CONFIG_HOME=directory + "/config",
                   XDG_STATE_HOME=directory + "/state", SHELL="/bin/sh", TERM="xterm-256color")
        (root / "config/fux").mkdir(parents=True)
        (root / "config/fux/config.toml").write_text('default-command = { argv = ["/bin/sh"] }\n')
        started = time.monotonic()
        server = subprocess.Popen([binary, "serve", "--name", "default"], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        sock_path = root / "fux/default.attach.sock"
        try:
            deadline = time.monotonic() + 10
            peer = None
            while time.monotonic() < deadline and peer is None:
                if sock_path.exists():
                    try:
                        candidate = socket.socket(socket.AF_UNIX)
                        candidate.settimeout(5)
                        candidate.connect(str(sock_path))
                        send(candidate, dict(type="hello", version=args.version, rows=24, columns=80))
                        hello = receive(candidate)
                        assert hello == {"hello": {"version": args.version}}, hello
                        peer = candidate
                    except (ConnectionRefusedError, FileNotFoundError):
                        time.sleep(0.005)
                else:
                    time.sleep(0.005)
                if server.poll() is not None:
                    raise RuntimeError(server.stderr.read().decode())
            assert peer is not None, "server did not start"
            startup = time.monotonic() - started
            # Wait for the shell prompt to settle.
            peer.settimeout(2)
            try:
                while True:
                    receive(peer)
            except (socket.timeout, TimeoutError):
                pass
            rss_start = rss_kib(server.pid)
            cpu_before = cpu_seconds(server.pid)
            wake_before = voluntary_switches(server.pid)
            time.sleep(10)
            cpu_after = cpu_seconds(server.pid)
            wake_after = voluntary_switches(server.pid)
            idle_cpu = cpu_after - cpu_before
            wakeups = None if wake_before is None else wake_after - wake_before
            latencies = []
            peer.settimeout(10)
            for index in range(args.samples):
                marker = f"MARK{index:03d}"
                begin = time.monotonic()
                send(peer, dict(type="input", bytes=list(f"printf {marker}Z\\\\n\n".encode())))
                while True:
                    message = receive(peer)
                    if marker + "Z" in frame_text(message):
                        break
                latencies.append(time.monotonic() - begin)
                # drain the echo of the following prompt
                peer.settimeout(0.2)
                try:
                    while True:
                        receive(peer)
                except (socket.timeout, TimeoutError):
                    pass
                peer.settimeout(10)
            send(peer, dict(type="input", bytes=list(b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURSTDONE\\\\n\n")))
            burst_start = time.monotonic()
            while True:
                message = receive(peer)
                if "BURSTDONE" in frame_text(message):
                    break
            burst = time.monotonic() - burst_start
            rss_after = rss_kib(server.pid)
            print(json.dumps({
                "binary": binary,
                "startup_s": round(startup, 4),
                "idle_cpu_s_per_10s": round(idle_cpu, 3),
                "idle_wakeups_per_10s": wakeups if wakeups is not None else "n/a",
                "rss_start_kib": rss_start,
                "rss_after_burst_kib": rss_after,
                "burst_20000_lines_s": round(burst, 3),
                "latency_median_ms": round(statistics.median(latencies) * 1000, 2),
                "latency_p95_ms": round(sorted(latencies)[int(len(latencies) * 0.95) - 1] * 1000, 2),
            }, indent=2))
            peer.close()
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()


if __name__ == "__main__":
    main()
