#!/usr/bin/env python3
"""Runs the scripted tier: every task, for each tool, N times, each run in a
process of its own with a server of its own, a work directory of its own, and
a hard timeout. Results go to a JSON file, one record per run; `report.py`
turns them into tables.

    python3 -I run.py --tools fux,tmux,zellij,herdr --repeats 10 --out results/base.json

Nothing here reaches the user's own sessions: each worker gets an environment
of its own (`isolated_env`), checked before anything runs.
"""
import argparse
import concurrent.futures
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
SCRATCH = os.path.expanduser("~/.cache/claude-tmp/agentbench")
FX = f"python3 -I {HERE}/fixtures.py"
SPOILERS = ["FUX_SOCKET", "FUX_PANE", "TMUX", "TMUX_PANE", "ZELLIJ", "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID", "HERDR_ENV", "HERDR_PANE_ID", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID",
            "HERDR_CLIENT_SOCKET_PATH"]


def tasks(path=os.path.join(HERE, "tasks.toml")):
    with open(path, "rb") as f:
        return tomllib.load(f)["task"]


def isolated_env(slot):
    """An environment whose every socket and runtime directory is this
    slot's own, under the scratch directory."""
    base = os.path.join(SCRATCH, f"slot{slot}")
    env = {k: v for k, v in os.environ.items() if k not in SPOILERS}
    dirs = {
        "XDG_RUNTIME_DIR": "rt",
        "TMUX_TMPDIR": "tmux",
        "ZELLIJ_SOCKET_DIR": "zellij-sock",
        "ZELLIJ_CONFIG_DIR": "zellij-conf",
        "XDG_CONFIG_HOME": "config",
    }
    for key, sub in dirs.items():
        path = os.path.join(base, sub)
        os.makedirs(path, mode=0o700, exist_ok=True)
        os.chmod(path, 0o700)
        env[key] = path
    os.makedirs(os.path.join(base, "fux"), mode=0o700, exist_ok=True)
    os.chmod(os.path.join(base, "fux"), 0o700)
    env["FUX_SOCKET"] = os.path.join(base, "fux", "s.sock")
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    for key in ["XDG_RUNTIME_DIR", "FUX_SOCKET", "TMUX_TMPDIR", "ZELLIJ_SOCKET_DIR", "XDG_CONFIG_HOME"]:
        assert env[key].startswith(SCRATCH), f"{key} escapes the scratch directory"
    return env


def check(task, work):
    """Whether the run did what the task asked, and why not if not."""
    c = task["check"]
    read = lambda name: open(os.path.join(work, name), encoding="utf-8").read() if os.path.exists(os.path.join(work, name)) else None
    if "answer" in c or "answer_contains" in c:
        got = (read("answer.txt") or "").strip()
        if "answer" in c:
            return got == c["answer"], f"answer {got!r}, wanted {c['answer']!r}"
        return c["answer_contains"] in got, f"answer {got!r}, wanted {c['answer_contains']!r} in it"
    if "gone" in c:
        text = read(c["gone"]) or ""
        pids = [int(x) for x in text.split()]
        alive = [p for p in pids if _alive(p)]
        return bool(pids) and not alive, f"pids {pids}, alive {alive}"
    text = read(c["file"])
    if text is None:
        return False, f"no {c['file']}"
    rows = [l for l in text.split("\n") if l]
    if "first_line" in c:
        return bool(rows) and rows[0] == c["first_line"], f"first line {rows[:1]}"
    if "last_line" in c:
        return bool(rows) and rows[-1] == c["last_line"], f"last line {rows[-1:]}"
    if "lines" in c:
        return len(rows) == c["lines"], f"{len(rows)} lines"
    if "contains" in c:
        return c["contains"] in text, f"{text[-200:]!r}"
    if "equals" in c:
        return text == c["equals"], f"{text!r}"
    if "line_equals" in c:
        return c["line_equals"] in [r for r in text.split("\n")], f"{text[-200:]!r}"
    if "lines_exact" in c:
        return rows == c["lines_exact"], f"{rows}"
    if "bytes_hex" in c:
        return "".join(rows) == c["bytes_hex"], f"{rows}"
    if "last_width" in c:
        return bool(rows) and int(rows[-1].split("x")[0]) == c["last_width"], f"{rows[-3:]}"
    if "widths_under" in c:
        widths = [int(r.split("x")[0]) for r in rows]
        return len(rows) >= c["min_lines"] and all(w < c["widths_under"] for w in widths[-2:]), f"{rows}"
    return False, f"unknown check {c}"


def _alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        with open(f"/proc/{pid}/stat") as f:
            return f.read().split(") ")[1][0] != "Z"
    except OSError:
        return True


def reap(work):
    """Kills whatever a run's programs left, by the pids they wrote."""
    for name in ("pid.txt", "pids.txt"):
        path = os.path.join(work, name)
        if os.path.exists(path):
            for pid in open(path).read().split():
                try:
                    os.kill(int(pid), signal.SIGKILL)
                except (ProcessLookupError, ValueError, PermissionError):
                    pass


def cleanup_slot(tool, slot):
    """Stops whatever server a killed run left in its slot."""
    env = isolated_env(slot)
    cmds = {"fux": [[os.path.expanduser("~/code/fux/target/release/fux"), "kill-server"]],
            "tmux": [["tmux", "-L", "ab", "kill-server"]],
            "zellij": [["zellij", "kill-session", "ab"], ["zellij", "delete-session", "ab", "--force"]],
            "herdr": [["herdr", "--session", "ab", "server", "stop"]]}[tool]
    for cmd in cmds:
        subprocess.run(cmd, env=env, capture_output=True, timeout=20)


def driver_for(tool, work, env, fux_bin):
    import drivers
    if tool == "fux":
        # This PR's fux, if the binary has its commands; else as on master.
        new = subprocess.run([fux_bin, "help", "respawn"], capture_output=True, env=env).returncode == 0
        return (drivers.FuxAfter if new else drivers.Fux)(work, env, fux_bin)
    return {"tmux": drivers.Tmux, "zellij": drivers.Zellij, "herdr": drivers.Herdr}[tool](work, env)


def one(tool, task_id, rep, slot, fux_bin):
    """One run, in this process: the result as a dict."""
    sys.path.insert(0, HERE)
    import drivers
    import solutions
    task = next(t for t in tasks() if t["id"] == task_id)
    work = os.path.join(SCRATCH, "runs", tool, task_id, str(rep))
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    os.chdir(work)
    env = isolated_env(slot)
    d = driver_for(tool, work, env, fux_bin)
    result = {"tool": tool, "task": task_id, "cat": task["cat"], "rep": rep}
    start = time.time()
    try:
        d.start()
        answer = getattr(solutions, task["recipe"])(d, task.get("args", {}), FX)
        if answer is not None:
            with open(os.path.join(work, "answer.txt"), "w") as f:
                f.write(str(answer))
        result["error"] = None
    except drivers.Unsupported as e:
        result["error"] = f"unsupported: {e}"
    except Exception as e:
        result["error"] = f"{type(e).__name__}: {e}"[:400]
        result["trace"] = traceback.format_exc()[-1500:]
    result["seconds"] = round(time.time() - start, 2)
    ok, why = check(task, work)
    result.update(ok=ok and not str(result["error"] or "").startswith("unsupported"), why=why,
                  calls=d.calls, sleep_s=round(d.sleep_s, 2), polls=d.polls, log=d.log[-60:])
    try:
        d.stop()
    except Exception:
        pass
    reap(work)
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tools", default="fux,tmux,zellij,herdr")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--repeats", type=int, default=10)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--timeout", type=int, default=90)
    ap.add_argument("--fux-bin", default=os.path.expanduser("~/code/fux/target/release/fux"))
    ap.add_argument("--skip-expensive", action="store_true")
    ap.add_argument("--out", default=os.path.join(HERE, "results", "scripted.json"))
    ap.add_argument("--one", nargs=4, metavar=("TOOL", "TASK", "REP", "SLOT"))
    args = ap.parse_args()
    if args.one:
        tool, task, rep, slot = args.one
        print(json.dumps(one(tool, task, int(rep), int(slot), args.fux_bin)))
        return
    chosen = [t for t in tasks() if (not args.tasks or t["id"] in args.tasks.split(","))
              and not (args.skip_expensive and t.get("expensive"))]
    jobs = [(tool, t["id"], rep) for tool in args.tools.split(",") for t in chosen
            for rep in range(args.repeats if not t.get("expensive") else 1)]
    slots = list(range(args.workers))
    results = []

    def run(job, slot):
        tool, task, rep = job
        cmd = [sys.executable, "-I", os.path.abspath(__file__), "--fux-bin", args.fux_bin,
               "--one", tool, task, str(rep), str(slot)]
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=args.timeout,
                                 env=isolated_env(slot))
            return json.loads(out.stdout.strip().split("\n")[-1])
        except subprocess.TimeoutExpired:
            cleanup_slot(tool, slot)
            return {"tool": tool, "task": task, "rep": rep, "ok": False, "error": "harness timeout",
                    "why": "", "calls": 0, "sleep_s": 0, "polls": 0, "seconds": args.timeout}
        except Exception as e:
            return {"tool": tool, "task": task, "rep": rep, "ok": False, "error": f"harness: {e}",
                    "why": "", "calls": 0, "sleep_s": 0, "polls": 0, "seconds": 0}

    free = list(slots)
    with concurrent.futures.ThreadPoolExecutor(args.workers) as pool:
        pending = {}
        queue = list(jobs)
        while queue or pending:
            while queue and free:
                slot = free.pop()
                job = queue.pop(0)
                pending[pool.submit(run, job, slot)] = slot
            done, _ = concurrent.futures.wait(pending, return_when=concurrent.futures.FIRST_COMPLETED)
            for fut in done:
                free.append(pending.pop(fut))
                r = fut.result()
                results.append(r)
                mark = "ok " if r["ok"] else "FAIL"
                print(f"{mark} {r['tool']:<6} {r['task']:<26} {r.get('seconds', 0):>6}s "
                      f"calls={r['calls']} sleep={r['sleep_s']} {r.get('error') or ''} "
                      f"{'' if r['ok'] else r.get('why', '')[:120]}", flush=True)
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(results, f, indent=1)
    print(f"{sum(r['ok'] for r in results)}/{len(results)} runs ok -> {args.out}")


if __name__ == "__main__":
    main()
