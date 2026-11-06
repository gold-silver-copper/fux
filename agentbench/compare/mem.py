#!/usr/bin/env python3
"""Server memory with a full 50,000-line history: a pane fills its history
with 60,000 lines of 80 characters, then the server's resident set is read.

  mem.py BIN...
"""
import os, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fuxwait import agent_tab, run, string, until_text  # noqa: E402

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
    e.update(XDG_RUNTIME_DIR=d, FUX_SOCKET=os.path.join(d, "s"))
    run(bin_, e, "(start)")
    pid = int(run(bin_, e, '(get (status) "server-process-id")').stdout)
    base = rss(pid)
    fill = "python3 -c \"import sys\nfor i in range(60000): sys.stdout.write('%05d ' % i + 'x' * 73 + '\\n')\"; read x"
    started = run(bin_, e, agent_tab(["sh", "-c", fill], history=50000 * (24 + 8 * 79)))
    pane = until_text(bin_, e, started.stdout, "59999 ", 120)
    kept = run(bin_, e, f"(capture (all-history (pane {string(pane)})))").stdout.count("\n")
    time.sleep(1)
    full = rss(pid)
    print(f"  {os.path.basename(bin_)}: {kept} lines kept")
    if pid:
        try:
            os.kill(pid, 15)
        except OSError:
            pass
    return base, full


for b in sys.argv[1:]:
    base, full = measure(b)
    print(f"{os.path.basename(b):12} empty {base:6.1f} MiB   one pane, 50,000-line history full {full:6.1f} MiB   (+{full - base:.1f})")
