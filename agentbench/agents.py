#!/usr/bin/env python3
"""The agent tier: a fresh agent per task and tool, told only the goal and how
to learn the tool, in a server of its own. Each agent is a `claude -p`
process (a small model) that this runner starts with an environment it
builds from nothing: a runtime directory and sockets of the run's own, and the
tools behind wrappers that log every call. What it did is read from its stream
of tool calls, not from what it says; whether it succeeded, from the task's
checker.

    python3 -I agents.py --tools fux --tasks all --out results/agents-fux.json
    python3 -I agents.py --tools tmux,zellij,herdr --sample 40 --out results/agents-others.json

An agent cannot reach the user's sessions: the environment carries no socket
but the run's, and its runtime directory, where every tool looks by default,
is the run's own.
"""
import argparse
import concurrent.futures
import json
import os
import random
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import run  # noqa: E402

SCRATCH = run.SCRATCH
CLAUDE = shutil.which("claude") or os.path.expanduser("~/.local/bin/claude")
REAL = {name: shutil.which(name) for name in ("tmux", "zellij", "herdr", "sleep")}

LEARN = {
    "fux": "fux, a terminal multiplexer. Learn it with `fux help agents` (start there) and "
           "`fux help COMMAND`. A private fux server is already running for you: FUX_SOCKET points "
           "to it, and nothing in this environment reaches anyone else's fux session, so you may "
           "use any fux command, kill-server included.",
    "tmux": "tmux. Learn it with `man tmux` or `tmux list-commands`. A private tmux server is "
            "already running for you: always pass `-L ab` (for example `tmux -L ab list-panes -a`).",
    "zellij": "zellij. Learn it with `zellij --help` and `zellij action --help`. A private "
              "zellij session named `ab` is already running for you: pass `--session ab` "
              "(for example `zellij --session ab action list-panes --json`).",
    "herdr": "herdr. Learn it with `herdr --skill`, `herdr --help` and `herdr pane`. A private "
             "herdr session named `ab` is already running for you: pass `--session ab` to every "
             "command (for example `herdr --session ab pane list`). You are allowed to control "
             "it although HERDR_ENV is not set: it is a test session made for you.",
}

PROMPT = """You are testing how well an AI agent can drive terminal programs with {learn}

Your task:
{goal}

Rules:
- Run the program inside the multiplexer (in one of its panes), not directly in your own shell,
  and interact with it only through the multiplexer.
- Work in the current directory ({work}); the program's files go there. Write any answer to
  answer.txt there, with nothing else in it.
- Use only the multiplexer's own commands to start, drive, read and wait for the program.
- When you are done, end with a line `FRICTION:` followed by anything about the multiplexer that
  made the task harder than it should be (or `none`).
"""


def shims(slot, tool, fux_bin, log):
    """A directory of wrappers that log each call, first in PATH."""
    path = os.path.join(SCRATCH, f"agent-slot{slot}", "bin")
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path)
    real = dict(REAL, fux=fux_bin)
    for name, target in real.items():
        if not target:
            continue
        with open(os.path.join(path, name), "w") as f:
            f.write(f'#!/bin/sh\nprintf "%s\\t%s\\t%s\\n" "$(date +%s.%N)" "{name}" "$*" >> "{log}"\n'
                    f'exec "{target}" "$@"\n')
        os.chmod(os.path.join(path, name), 0o755)
    return path


def agent_env(slot, tool, fux_bin, work):
    base = run.isolated_env(100 + slot)
    log = os.path.join(work, ".calls.log")
    bin_dir = shims(slot, tool, fux_bin, log)
    keep = ["XDG_RUNTIME_DIR", "FUX_SOCKET", "TMUX_TMPDIR", "ZELLIJ_SOCKET_DIR", "ZELLIJ_CONFIG_DIR",
            "XDG_CONFIG_HOME"]
    env = {k: base[k] for k in keep}
    env.update(HOME=os.environ["HOME"], USER=os.environ.get("USER", ""), LANG="en_US.UTF-8",
               TERM="xterm-256color",
               PATH=":".join([bin_dir, os.path.dirname(CLAUDE), "/usr/local/bin", "/usr/bin", "/bin"]))
    for key in keep:
        assert env[key].startswith(SCRATCH), key
    return env


def one(tool, task, slot, fux_bin, model, timeout, tag="run"):
    work = os.path.join(SCRATCH, "agent-runs", tag, tool, task["id"])
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    env = agent_env(slot, tool, fux_bin, work)
    if task.get("args", {}).get("setup_lines"):
        with open(os.path.join(work, "data.txt"), "w") as f:
            f.write("".join(f"{i}\n" for i in range(task["args"]["setup_lines"])))
    if task["recipe"] == "less_search":
        with open(os.path.join(work, "lines.data"), "w") as f:
            f.write("".join(f"row {i}{' needle here' if i == 338 else ''}\n" for i in range(1, 600)))
    if task["recipe"] == "nvim_edit":
        open(os.path.join(work, "edit.txt"), "w").close()
    d = run.driver_for(tool, work, env, fux_bin)
    d.start()
    goal = task["goal"].replace("{fx}", run.FX)
    prompt = PROMPT.format(learn=LEARN[tool], goal=goal, work=work)
    started = time.time()
    record = {"tool": tool, "task": task["id"], "cat": task["cat"]}
    try:
        out = subprocess.run([CLAUDE, "-p", "--model", model, "--output-format", "stream-json",
                              "--verbose", "--permission-mode", "bypassPermissions",
                              "--max-turns", "60", prompt],
                             cwd=work, env=env, capture_output=True, text=True, timeout=timeout)
        stream = out.stdout
        record["timed_out"] = False
    except subprocess.TimeoutExpired as e:
        stream = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        record["timed_out"] = True
    record["seconds"] = round(time.time() - started, 1)
    commands, final, usage = [], "", {}
    for line in stream.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("type") == "assistant":
            for part in msg.get("message", {}).get("content", []):
                if part.get("type") == "tool_use":
                    commands.append(part.get("input", {}).get("command") or json.dumps(part.get("input"))[:300])
        if msg.get("type") == "result":
            final = msg.get("result") or ""
            usage = {"cost_usd": msg.get("total_cost_usd"), "turns": msg.get("num_turns"),
                     "tokens_out": (msg.get("usage") or {}).get("output_tokens")}
    log = open(os.path.join(work, ".calls.log")).read().splitlines() if os.path.exists(os.path.join(work, ".calls.log")) else []
    calls = [l.split("\t") for l in log]
    record.update(
        tool_calls=sum(1 for c in calls if len(c) > 1 and c[1] == tool),
        sleeps=sum(1 for c in calls if len(c) > 1 and c[1] == "sleep"),
        sleep_s=round(sum(_seconds(c[2]) for c in calls if len(c) > 2 and c[1] == "sleep"), 1),
        bash_calls=len(commands),
        helper_scripts=sum(1 for c in commands if re.search(r"cat\s*>\s*\S+\.(sh|py)|\.(sh|py)\s*<<|chmod \+x", c)),
        errors=sum(1 for c in commands if "Error" in c),
        friction=final.split("FRICTION:")[-1].strip()[:600] if "FRICTION:" in final else "",
        commands=commands[-40:],
        **usage,
    )
    ok, why = run.check(task, work)
    record.update(ok=ok, why=why)
    try:
        d.stop()
    except Exception:
        pass
    run.reap(work)
    return record


def _seconds(text):
    try:
        return float(text.split()[0])
    except (ValueError, IndexError):
        return 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tools", default="fux")
    ap.add_argument("--tasks", default="all")
    ap.add_argument("--sample", type=int, default=0, help="a stratified sample of N tasks")
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--slot-base", type=int, default=0, help="first slot, so runs side by side do not share one")
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--fux-bin", default=os.path.join(SCRATCH, "bins", "fux-base"))
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    tasks = run.tasks()
    if args.tasks != "all":
        tasks = [t for t in tasks if t["id"] in args.tasks.split(",")]
    if args.sample:
        tasks = sample(tasks, args.sample)
    jobs = [(tool, t) for tool in args.tools.split(",") for t in tasks]
    results = []
    if os.path.exists(args.out):
        results = json.load(open(args.out))
    done = {(r["tool"], r["task"]) for r in results}
    jobs = [j for j in jobs if (j[0], j[1]["id"]) not in done]
    print(f"{len(jobs)} agent runs to do", flush=True)
    free = list(range(args.slot_base, args.slot_base + args.workers))
    with concurrent.futures.ThreadPoolExecutor(args.workers) as pool:
        pending = {}
        while jobs or pending:
            while jobs and free:
                slot = free.pop()
                tool, task = jobs.pop(0)
                tag = os.path.splitext(os.path.basename(args.out))[0]
                pending[pool.submit(one, tool, task, slot, args.fux_bin, args.model, args.timeout, tag)] = slot
            finished, _ = concurrent.futures.wait(pending, return_when=concurrent.futures.FIRST_COMPLETED)
            for fut in finished:
                free.append(pending.pop(fut))
                try:
                    r = fut.result()
                except Exception as e:
                    print(f"harness error: {e}", flush=True)
                    continue
                results.append(r)
                with open(args.out, "w") as f:
                    json.dump(results, f, indent=1)
                print(f"{'ok ' if r['ok'] else 'FAIL'} {r['tool']:<6} {r['task']:<26} {r['seconds']:>6}s "
                      f"calls={r['tool_calls']} sleeps={r['sleeps']} bash={r['bash_calls']} "
                      f"${r.get('cost_usd') or 0:.3f} {'' if r['ok'] else r['why'][:80]}", flush=True)


def sample(tasks, n):
    """A sample of N tasks, as many from each category as its share,
    always the same."""
    rng = random.Random(42)
    by_cat = {}
    for t in tasks:
        by_cat.setdefault(t["cat"], []).append(t)
    chosen = []
    for cat, group in by_cat.items():
        k = max(1, round(n * len(group) / len(tasks)))
        chosen += rng.sample(group, min(k, len(group)))
    return chosen[:n]


if __name__ == "__main__":
    main()
