#!/usr/bin/env python3
"""Speed of two fux builds on the same jobs, each on a private server:
PR #160's table, plus an agent's typical action (a 40-character command
and Enter) in bash and in a program that reads keys raw (python's REPL).
Medians of N rounds, in milliseconds, the builds alternating. Both builds
must speak one-word versions (`%2:0.57.0.3.0:RUN`, waited from with
`str V changed wait`): this fux or later.

  speed.py BASE_BIN CAND_BIN [ROUNDS]
"""
import os, shutil, statistics, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fuxwait import run, until_text  # noqa: E402

D = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "C", "work")


def env(tag):
    d = os.path.join(D, tag)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, mode=0o700)
    e = {k: v for k, v in os.environ.items() if k not in ("FUX_SOCKET", "FUX_PANE", "FUX_SERVER")}
    e.update(XDG_RUNTIME_DIR=d, FUX_SOCKET=os.path.join(d, "s"))
    return e


def ms(f):
    t = time.perf_counter(); f(); return 1000 * (time.perf_counter() - t)


def jobs(bin_, tag):
    e = env(tag)
    out = {}
    out["start"] = ms(lambda: run(bin_, e, "start"))
    out["ls (round trip)"] = ms(lambda: run(bin_, e, "ls"))
    start = lambda: run(bin_, e, "agent-opts", "80", "24", "size", "str", "sh", "program", "str", "-c", "arg",
                        "str", "sleep 0.2; echo READY; exec cat", "arg", "run").stdout
    panes = []
    # Started and waited for in one measure: the start is part of the work.
    out["start and wait for a text (0.2s program)"] = ms(lambda: panes.append(until_text(bin_, e, start(), "READY")))
    pane = panes[0]
    out["capture of a quiet pane"] = ms(lambda: run(bin_, e, pane, "capture"))
    out["capture all-history json"] = ms(lambda: run(bin_, e, pane, "all-history", "capture", "json"))
    def fan():
        for i in range(30):
            until_text(bin_, e, run(bin_, e, "agent-opts", "80", "24", "size", "str", f"f{i}", "name", "str", "sh", "program", "str", "-c",
                                    "arg", "str", f"echo up-{i}; exec cat", "arg", "run").stdout, f"up-{i}")
    out["30 panes started, each waited for its text"] = ms(fan)
    command = "echo 'the quick brown fox jumps over 42'"  # 40 characters
    assert len(command) == 40
    sh = run(bin_, e, "agent-opts", "80", "24", "size", "str", "bash", "program", "str", "--norc", "arg", "str", "-i", "arg", "run").stdout.split(":")[0]
    out["40-char command + Enter, bash"] = ms(lambda: run(bin_, e, sh, "str", command, "text", "str", "Enter", "key", "send-keys"))
    py = until_text(bin_, e, run(bin_, e, "agent-opts", "80", "24", "size", "str", "python3", "program", "str", "-q", "arg", "str", "-i", "arg", "run").stdout, ">>>")
    line = "x = 'the quick brown fox jumps over 42'"
    line = line + " " * (40 - len(line))
    out["40-char line + Enter, python REPL (raw)"] = ms(lambda: run(bin_, e, py, "str", line, "text", "str", "Enter", "key", "send-keys"))
    run(bin_, e, "force-kill-server")
    return out


def main():
    base, new = sys.argv[1], sys.argv[2]
    rounds = int(sys.argv[3]) if len(sys.argv) > 3 else 10
    res = {"master": [], "branch": []}
    for _ in range(rounds):
        res["master"].append(jobs(base, "master"))
        res["branch"].append(jobs(new, "branch"))
    print(f"{'job':42} {'master ms':>10} {'branch ms':>10} {'b/m':>6}")
    for k in res["master"][0]:
        b = statistics.median(x[k] for x in res["master"]); n = statistics.median(x[k] for x in res["branch"])
        print(f"{k:42} {b:10.1f} {n:10.1f} {n / b:6.2f}")


if __name__ == "__main__":
    main()
