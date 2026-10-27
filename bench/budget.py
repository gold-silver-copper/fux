#!/usr/bin/env python3
"""One Linux real-server budget probe. No timings are pass/fail gates."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import platform
import select
import signal
import subprocess
import time


def run(argv, env):
    result = subprocess.run(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{argv!r}: {result.stderr.decode(errors='replace')}")
    return result.stdout


def status(pid):
    fields = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in ("VmRSS", "VmHWM", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"):
            fields[key] = int(value.split()[0])
    # comm may contain spaces or parentheses; fields after it start at #3.
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    fields["cpu_ticks"] = int(stat[11]) + int(stat[12])
    return fields


class Counter:
    """Only this server PID, enabled/disabled with perf's acknowledged control."""
    def __init__(self, pid, directory, label):
        self.raw = directory / f"{label}.perf"
        self.control = directory / f"{label}.control"
        self.ack = directory / f"{label}.ack"
        for path in (self.control, self.ack):
            os.mkfifo(path, 0o600)
        self.cfd = os.open(self.control, os.O_RDWR)
        self.afd = os.open(self.ack, os.O_RDWR | os.O_NONBLOCK)
        self.child = subprocess.Popen([
            os.environ.get("FUX_PERF", "perf"), "stat", "-v", "-x", ",",
            "--no-scale", "-e", "instructions:u", "-p", str(pid),
            "--delay=-1", f"--control=fifo:{self.control},{self.ack}",
            "-o", str(self.raw),
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            self.command("enable")
        except BaseException:
            self.close()
            raise

    def command(self, command):
        os.write(self.cfd, (command + "\n").encode())
        if not select.select([self.afd], [], [], 5)[0]:
            raise RuntimeError(f"perf did not acknowledge {command}: {self.raw.read_text()}")
        os.read(self.afd, 4096)

    def close(self):
        if self.child.poll() is None:
            self.child.send_signal(signal.SIGINT)
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait()
        os.close(self.cfd)
        os.close(self.afd)

    def finish(self):
        try:
            self.command("disable")
        finally:
            self.close()
        # Same whole-count rule as count.rs: verbose enabled/running must
        # match exactly; CSV alone rounds its share to two decimal places.
        raw = self.raw.read_text()
        counts = []
        csv = []
        for line in raw.splitlines():
            event, sep, numbers = line.partition(": ")
            if sep and (event.startswith("instructions") or "/instructions/" in event):
                value, enabled, running = map(int, numbers.split())
                if running:
                    if enabled != running:
                        raise RuntimeError(f"partial perf counter: {line}")
                    counts.append(value)
                elif value:
                    raise RuntimeError(f"perf counted without running: {line}")
            parts = line.split(",")
            if len(parts) >= 5 and (parts[2].startswith("instructions") or "/instructions/" in parts[2]):
                if parts[0] == "<not counted>":
                    continue
                csv.append(int(parts[0]))
        if not counts or sum(counts) != sum(csv):
            raise RuntimeError(f"no whole instruction count: {raw}")
        return sum(counts)


def probe(binary, directory, history):
    directory.mkdir(mode=0o700)
    runtime = directory / "runtime"
    runtime.mkdir(mode=0o700)
    env = dict(os.environ)
    for key in ("FUX_SOCKET", "FUX_PANE", "FUX_SERVER", "FUX", "ENV", "BASH_ENV"):
        env.pop(key, None)
    env.update(HOME=str(directory), XDG_RUNTIME_DIR=str(runtime),
               XDG_CONFIG_HOME=str(directory / "config"),
               FUX_SOCKET=str(directory / "s"), SHELL="/bin/sh", PS1="$ ")
    log = (directory / "server.log").open("wb")
    server = subprocess.Popen([binary, "__serve", env["FUX_SOCKET"]], env=env,
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
    cli = lambda *args: run([binary, *args], env)
    try:
        deadline = time.monotonic() + 10
        while not Path(env["FUX_SOCKET"]).exists():
            if server.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError("private server failed to start")
            time.sleep(0.01)
        cli("str", "history-bytes", str(history), "set")
        # Twice the text needed to fill history, variable row lengths.
        block = b"".join(b"x" * n + b"\r\n" for n in range(1, 120))
        fixture = directory / "fill"
        fixture.write_bytes(block * ((history // 2) // len(block) + 1))
        marker = "FUX_BUDGET_DONE"
        emitter = directory / "emit.py"
        emitter.write_text(
            "import pathlib, sys, time\n"
            "sys.stdin.readline()\n"
            f"sys.stdout.buffer.write(pathlib.Path({str(fixture)!r}).read_bytes())\n"
            f"sys.stdout.buffer.write(b'\\r\\n{marker}\\r\\n')\n"
            "sys.stdout.buffer.flush()\n"
            "sys.stdin.readline()\n")
        pane = cli("opts", "120", "40", "size", "str", "/usr/bin/python3", "program",
                   "str", str(emitter), "arg", "run").decode().strip().split(":")[0]
        # Pin the single server thread; the environment records the CPU.
        cpu = min(os.sched_getaffinity(server.pid))
        os.sched_setaffinity(server.pid, {cpu})
        counter = Counter(server.pid, directory, "fill")
        try:
            started = time.monotonic()
            cli(pane, "str", "Enter", "key", "send-keys")
            deadline = time.monotonic() + 30
            while marker.encode() not in cli(pane, "capture"):
                if time.monotonic() > deadline:
                    raise RuntimeError("fill did not reach its completion marker")
                time.sleep(0.01)
            fill_seconds = time.monotonic() - started
        finally:
            fill_instructions = counter.finish()
        # Settled idle server, no observations sent during this interval.
        time.sleep(0.1)
        idle_before = status(server.pid)
        time.sleep(10)
        idle_after = status(server.pid)
        before = status(server.pid)
        counter = Counter(server.pid, directory, "capture")
        try:
            started = time.monotonic()
            reply = cli(pane, "all-history", "capture")
            seconds = time.monotonic() - started
        finally:
            instructions = counter.finish()
        after = status(server.pid)
        (directory / "capture.txt").write_bytes(reply)
        return dict(history_bytes=history, cpu=cpu, reply_bytes=len(reply),
                    cli_wall_seconds=seconds, server_instructions=instructions,
                    rss_before_kib=before["VmRSS"], peak_rss_kib=after["VmHWM"],
                    peak_before_kib=before["VmHWM"],
                    fill=dict(bytes=fixture.stat().st_size + len(marker) + 4,
                              wall_seconds=fill_seconds, server_instructions=fill_instructions,
                              instructions_per_byte=fill_instructions / (fixture.stat().st_size + len(marker) + 4)),
                    idle_10_seconds={key: idle_after[key] - idle_before[key] for key in
                                     ("cpu_ticks", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")})
    finally:
        if server.poll() is None:
            with contextlib.suppress(Exception):
                cli("kill-server")
            if server.poll() is None:
                server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=lambda p: str(Path(p).resolve()))
    parser.add_argument("directory", type=lambda p: Path(p).resolve(), help="new private directory, short enough for a Unix socket")
    parser.add_argument("--history-mib", nargs="+", type=int, default=[8, 64])
    args = parser.parse_args()
    args.directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    results = dict(environment=dict(platform=platform.platform(), loadavg=os.getloadavg(),
                                   perf=run([os.environ.get("FUX_PERF", "perf"), "--version"], os.environ).decode().strip(),
                                   note="One sample each on shared hardware. CLI wall includes startup/transport/formatting; server counts include request handling. RSS high-water includes setup. Fill count includes completion polling. Idle context switches proxy wakeups; ticks are quantized."), captures=[])
    for index, mib in enumerate(args.history_mib):
        if not 0 < mib <= 1024:
            raise ValueError("history must be 1..1024 MiB")
        results["captures"].append(probe(args.binary, args.directory / str(index), mib << 20))
        (args.directory / "budget.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
