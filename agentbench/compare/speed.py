#!/usr/bin/env python3
"""Speed of two fux builds on the same jobs, each on a private server:
PR #160's table, plus an agent's typical action (a 40-character command
and Enter) in bash and in a program that reads keys raw (python's REPL).
Medians of N rounds, in milliseconds, the builds alternating. Both builds
must speak one-word versions (`%2:0.57.0.3.0:RUN`, waited from with
`(wait (changed V))`): this fux or later.

  speed.py BASE_BIN CAND_BIN [ROUNDS]
"""
import os, shutil, statistics, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fuxwait import agent_tab, run, string, until_text  # noqa: E402

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
    out["start"] = ms(lambda: run(bin_, e, "(start)"))
    out["ls (round trip)"] = ms(lambda: run(bin_, e, "(list-session)"))
    start = lambda: run(bin_, e, agent_tab(["sh", "-c", "sleep 0.2; echo READY; exec cat"])).stdout
    panes = []
    out["start and wait for a text (0.2s program)"] = ms(lambda: panes.append(until_text(bin_, e, start(), "READY")))
    pane = panes[0]
    target = f"(pane {string(pane)})"
    out["capture of a quiet pane"] = ms(lambda: run(bin_, e, f"(capture {target})"))
    out["capture all-history"] = ms(lambda: run(bin_, e, f"(capture (all-history {target}))"))
    def fan():
        for i in range(30):
            until_text(bin_, e, run(bin_, e, agent_tab(["sh", "-c", f"echo up-{i}; exec cat"])).stdout, f"up-{i}")
    out["30 panes started, each waited for its text"] = ms(fan)
    command = "echo 'the quick brown fox jumps over 42'"  # 40 characters
    assert len(command) == 40
    sh = run(bin_, e, agent_tab(["bash", "--norc", "-i"])).stdout.split(":")[0]
    out["40-char command + Enter, bash"] = ms(lambda: run(bin_, e, f"(line (pane {string(sh)}) {string(command)})"))
    py = until_text(bin_, e, run(bin_, e, agent_tab(["python3", "-q", "-i"])).stdout, ">>>")
    line = "x = 'the quick brown fox jumps over 42'"
    line = line + " " * (40 - len(line))
    out["40-char line + Enter, python REPL (raw)"] = ms(lambda: run(bin_, e, f"(line (pane {string(py)}) {string(line)})"))
    subprocess.run(["sh", "-c", 'pid=$("$0" \'(get (status) "server-process-id")\') && kill "$pid"', bin_],
                   env=e, capture_output=True, timeout=20)
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
