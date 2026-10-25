#!/usr/bin/env python3
"""Server memory with a full 50,000-line history: a pane fills its history
with 60,000 lines of 80 characters, then the server's resident set is read.

  mem.py BIN...
"""
import os, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fuxwait import run, until_text  # noqa: E402

D = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "C", "mwork")


def rss(pid):
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return 0.0


def measure(bin_):
    d = os.path.join(D, os.path.basename(bin_))
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, mode=0o700)
    e = {k: v for k, v in os.environ.items() if k not in ("FUX_SOCKET", "FUX_PANE", "FUX_SERVER")}
    e.update(XDG_RUNTIME_DIR=d, FUX_SOCKET=os.path.join(d, "s"), XDG_STATE_HOME=os.path.join(d, "st"))
    run(bin_, e, "start")
    # 50,000 rows of 79 cells: 24 bytes a row and 8 a cell; a fux before
    # history-bytes counts rows.
    set_ = run(bin_, e, "str", "history-bytes", str(50000 * (24 + 8 * 79)), "set")
    if set_.returncode != 0:
        set_ = run(bin_, e, "str", "history-lines", "50000", "set")
    if set_.returncode != 0:
        sys.exit(f"{bin_}: history could not be set: {set_.stderr}")
    def server():
        for p in os.listdir("/proc"):
            try:
                cmd = open(f"/proc/{p}/cmdline", "rb").read().split(b"\0")
            except OSError:
                continue
            if b"__serve" in cmd and e["FUX_SOCKET"].encode() in cmd:
                return int(p)
    pid = server()
    base = rss(pid)
    fill = "python3 -c \"import sys\nfor i in range(60000): sys.stdout.write('%05d ' % i + 'x' * 73 + '\\n')\"; read x"
    started = run(bin_, e, "opts", "80", "24", "size", "str", "sh", "program", "str", "-c", "arg", "str", fill, "arg", "run")
    pane = until_text(bin_, e, started.stdout, "59999 ", 120)
    kept = run(bin_, e, pane, "all-history", "capture").stdout.count("\n")
    time.sleep(1)
    full = rss(pid)
    print(f"  {os.path.basename(bin_)}: {kept} lines kept")
    run(bin_, e, "force-kill-server")
    if pid:
        try:
            os.kill(pid, 9)
        except OSError:
            pass
    return base, full


for b in sys.argv[1:]:
    base, full = measure(b)
    print(f"{os.path.basename(b):12} empty {base:6.1f} MiB   one pane, 50,000-line history full {full:6.1f} MiB   (+{full - base:.1f})")
