#!/usr/bin/env python3
"""Gathers a set's specs from its writers' folders and checks each one
mechanically, outside any sandbox, on a private tmux server of its own:
the reference solution passes the checker (twice: deterministic), and an
empty work directory fails it. Writes SET/validation.json.

  validate.py SET [WORKERS]
"""
import concurrent.futures as cf
import glob
import importlib.util
import json
import os
import py_compile
import shutil
import subprocess
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
HERE = os.path.dirname(os.path.abspath(__file__))
AGENT = os.path.join(os.path.dirname(HERE), "agent")
sys.path[:0] = [HERE, AGENT]
import cxagent  # noqa: E402
import speccopy  # noqa: E402


KINDS = ["several-programs", "long-horizon", "recovery", "echo-traps", "overwritten-text", "reading",
         "full-screen", "layout-size", "sessions"]
NEEDS = ["start-program", "type-text", "special-keys", "paste", "wait-text", "wait-quiet", "wait-exit",
         "exit-status", "read-screen", "read-scrollback", "read-wrapped", "read-colors", "mouse",
         "set-size", "resize", "several-panes", "side-by-side", "pane-names", "detach-reattach", "bell",
         "signals", "kill-pane", "title", "cursor"]


def gather(set_name):
    specs = os.path.join(H, set_name, "specs")
    os.makedirs(specs, exist_ok=True)
    for d in sorted(glob.glob(os.path.join(H, set_name, "specgen", "job*", "T*"))):
        if os.path.isfile(os.path.join(d, "spec.json")):
            dst = os.path.join(specs, os.path.basename(d))
            if not os.path.exists(dst):
                speccopy.copy(d, dst)
    return sorted(glob.glob(os.path.join(specs, "T*")))


def load_check(spec_dir):
    sp = importlib.util.spec_from_file_location("c", os.path.join(spec_dir, "check.py"))
    m = importlib.util.module_from_spec(sp)
    sp.loader.exec_module(m)
    return m.check


def one_solve(spec_dir, rundir):
    shutil.rmtree(rundir, ignore_errors=True)
    work = os.path.join(rundir, "w")
    os.makedirs(work)
    task = cxagent.load_task(spec_dir)
    cxagent.setup(task, work)
    tmpdir = os.path.join(rundir, "t")
    os.makedirs(tmpdir, mode=0o700)
    # A HOME of its own: no tmux config of the user's is loaded.
    home = os.path.join(rundir, "home")
    os.makedirs(home)
    env = {"HOME": home, "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "en_US.UTF-8",
           "TERM": "xterm-256color", "TMUX_TMPDIR": tmpdir, "PYTHONDONTWRITEBYTECODE": "1"}
    # The reference solution's tmux calls are counted through a logging shim.
    shim = os.path.join(rundir, "bin")
    os.makedirs(shim)
    calls = os.path.join(rundir, "tmux-calls.log")
    with open(os.path.join(shim, "tmux"), "w") as f:
        f.write(f'#!/bin/sh\necho "$*" >> "{calls}"\nexec /usr/bin/tmux "$@"\n')
    os.chmod(os.path.join(shim, "tmux"), 0o755)
    tm = lambda *a: subprocess.run(["/usr/bin/tmux", "-L", "ab", *a], env=env, capture_output=True, timeout=30)
    tm("-f", "/dev/null", "new-session", "-d", "-s", "base", "-x", "120", "-y", "40")
    try:
        p = subprocess.run(["bash", os.path.join(spec_dir, "solve_tmux.sh"), work, cxagent.fixtures_dir(work)],
                           env=dict(env, PATH=shim + ":" + env["PATH"]), capture_output=True, text=True, timeout=900, cwd=work)
        solved = p.returncode
        err = (p.stderr or "")[-300:]
    except subprocess.TimeoutExpired:
        solved, err = "timeout", ""
    try:
        ok, why = load_check(spec_dir)(work)
    except Exception as e:
        ok, why = False, f"checker error {type(e).__name__}: {e}"
    tm("kill-server")
    cxagent.ab.reap(work)
    for pid_file in glob.glob(os.path.join(work, ".fx", "*.pid")):
        try:
            os.kill(int(open(pid_file).read().split()[0]), 9)
        except Exception:
            pass
    # Only calls that act count: reading the screen or listing panes in a
    # polling loop would make any task look long.
    reads = {"capture-pane", "capturep", "list-panes", "lsp", "list-sessions", "ls", "list-windows", "lsw",
             "display-message", "display", "has-session", "has", "show-options", "show", "show-buffer",
             "showb", "save-buffer", "saveb", "wait-for"}
    n = 0
    for line in (open(calls).read().splitlines() if os.path.exists(calls) else []):
        words = [w for w in line.split() if w]
        while words and words[0].startswith("-"):
            words = words[2:] if words[0] in ("-L", "-S", "-f") else words[1:]
        if words and words[0] not in reads:
            n += 1
    return {"solve_exit": solved, "ok": bool(ok), "why": str(why)[:200], "stderr": err, "tmux_calls": n}


def validate(spec_dir):
    sid = os.path.basename(spec_dir)
    r = {"id": sid, "problems": []}
    try:
        spec = json.load(open(os.path.join(spec_dir, "spec.json")))
        for k in ("id", "title", "category", "difficulty", "goal", "kinds", "needs"):
            if not spec.get(k):
                r["problems"].append(f"spec.json lacks {k}")
        if spec.get("id") != sid:
            r["problems"].append("id mismatch")
        for f in glob.glob(os.path.join(spec_dir, "fixtures", "*.py")) + [os.path.join(spec_dir, "check.py")]:
            compile(open(f).read(), f, "exec")
    except Exception as e:
        r["problems"].append(f"unreadable: {type(e).__name__}: {e}"[:200])
        return r
    import hashlib
    base = os.path.join(H, "validate-runs", sid + "-" + hashlib.sha1(spec_dir.encode()).hexdigest()[:8])
    try:
        empty = os.path.join(base, "empty")
        shutil.rmtree(empty, ignore_errors=True)
        os.makedirs(os.path.join(empty, ".fx"))
        ok, _ = load_check(spec_dir)(empty)
        if ok:
            r["problems"].append("an empty work directory passes the checker")
    except Exception as e:
        r["problems"].append(f"checker fails on an empty directory: {type(e).__name__}"[:200])
    r["runs"] = [one_solve(spec_dir, os.path.join(base, f"s{i}")) for i in (1, 2)]
    for i, run in enumerate(r["runs"], 1):
        if not run["ok"]:
            r["problems"].append(f"reference solution {i} fails: {run['why']}")
    if max(run["tmux_calls"] for run in r["runs"]) < 15:
        r["problems"].append("the reference solution makes fewer than 15 tmux calls that act (reads and listings not counted): not hard enough")
    try:
        kinds = spec.get("kinds") or []
        if len(set(kinds) & set(KINDS)) < 3:
            r["problems"].append(f"fewer than 3 of the listed kinds: {kinds}")
        bad = [n for n in spec.get("needs") or [] if n not in NEEDS]
        if bad:
            r["problems"].append(f"unknown needs: {bad}")
    except TypeError:
        r["problems"].append("kinds and needs must be lists of strings")
    shutil.rmtree(base, ignore_errors=True)
    return r


def main():
    if sys.argv[1] == "--one":
        # One spec, for a writer checking its own work through the proxy:
        # what failed, with the reference solution's stderr.
        r = validate(os.path.abspath(sys.argv[2]))
        for run in r.get("runs", []):
            run["stderr"] = run.get("stderr", "")[-1500:]
        print(json.dumps(r, indent=1))
        print("PASS" if not r["problems"] else "FAIL")
        sys.exit(0 if not r["problems"] else 1)
    set_name = sys.argv[1]
    workers = int(sys.argv[2]) if len(sys.argv) > 2 else 8
    dirs = gather(set_name)
    out = {}
    with cf.ThreadPoolExecutor(workers) as ex:
        for r in ex.map(validate, dirs):
            out[r["id"]] = r
            print(r["id"], "ok" if not r["problems"] else "; ".join(r["problems"])[:150], flush=True)
    json.dump(out, open(os.path.join(H, set_name, "validation.json"), "w"), indent=1)
    bad = [k for k, v in out.items() if v["problems"]]
    print(f"{len(out) - len(bad)}/{len(out)} specs pass mechanical validation")


if __name__ == "__main__":
    main()
