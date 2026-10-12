#!/usr/bin/env python3
"""Tables from the benchmark's results: per category and tool, for the
scripted tier (tasks solid in every repeat, runs that succeeded, commands,
sleeps and polls per task) and the agent tier (tasks done, tool calls,
sleeps, shell commands, cost, time), and the failures and friction behind
them.

    python3 -I report.py --scripted results/baseline-scripted.json --agents results/agents-base.json
"""
import argparse
import json
from collections import defaultdict

CATS = ["start", "shell", "wait", "tui", "read", "input", "agents", "size", "cleanup"]


def load(path):
    return json.load(open(path)) if path else []


def scripted_table(results, label):
    by = defaultdict(list)
    for r in results:
        by[(r["tool"], r.get("cat") or "?", r["task"])].append(r)
    tools = sorted({r["tool"] for r in results}, key=["fux", "tmux", "zellij", "herdr"].index)
    out = [f"### Scripted tier: {label}", "",
           "Per category: tasks that succeeded in every repeat / tasks; mean commands per run; "
           "mean seconds slept per run.", "",
           "| category | " + " | ".join(tools) + " |", "| --- |" + " --- |" * len(tools)]
    totals = defaultdict(lambda: [0, 0, 0.0, 0.0, 0])
    for cat in CATS:
        row = [cat]
        for tool in tools:
            tasks = {k: v for k, v in by.items() if k[0] == tool and k[1] == cat}
            solid = sum(1 for v in tasks.values() if all(r["ok"] for r in v))
            runs = [r for v in tasks.values() for r in v]
            calls = sum(r["calls"] for r in runs) / max(1, len(runs))
            sleep = sum(r["sleep_s"] for r in runs) / max(1, len(runs))
            t = totals[tool]
            t[0] += solid
            t[1] += len(tasks)
            t[2] += sum(r["calls"] for r in runs)
            t[3] += sum(r["sleep_s"] for r in runs)
            t[4] += len(runs)
            row.append(f"{solid}/{len(tasks)} · {calls:.1f} cmds · {sleep:.2f}s")
        out.append("| " + " | ".join(row) + " |")
    out.append("| **all** | " + " | ".join(
        f"**{t[0]}/{t[1]}** · {t[2] / max(1, t[4]):.1f} cmds · {t[3] / max(1, t[4]):.2f}s"
        for t in (totals[tool] for tool in tools)) + " |")
    out += ["", "Tasks not solid, per tool (runs ok / runs, and why the first failure failed):", ""]
    for tool in tools:
        bad = [(k[2], v) for k, v in sorted(by.items()) if k[0] == tool and not all(r["ok"] for r in v)]
        out.append(f"- **{tool}** ({len(bad)}): " + "; ".join(
            f"`{task}` {sum(r['ok'] for r in v)}/{len(v)}: "
            f"{next((r.get('error') or r.get('why') or '') for r in v if not r['ok'])[:70]}"
            for task, v in bad))
    return "\n".join(out)


def agent_table(results, label):
    tools = sorted({r["tool"] for r in results}, key=["fux", "tmux", "zellij", "herdr"].index)
    out = [f"### Agent tier: {label}", "",
           "Per category: tasks done (by the checker) / tasks run; mean multiplexer commands; "
           "mean `sleep` calls; mean shell commands per task.", "",
           "| category | " + " | ".join(tools) + " |", "| --- |" + " --- |" * len(tools)]
    for cat in CATS + ["all"]:
        row = [f"**{cat}**" if cat == "all" else cat]
        for tool in tools:
            rs = [r for r in results if r["tool"] == tool and (cat == "all" or r["cat"] == cat)]
            if not rs:
                row.append("–")
                continue
            n = len(rs)
            row.append(f"{sum(r['ok'] for r in rs)}/{n} · {sum(r['tool_calls'] for r in rs) / n:.1f} cmds · "
                       f"{sum(r['sleeps'] for r in rs) / n:.1f} sleeps · {sum(r['bash_calls'] for r in rs) / n:.1f} sh")
        out.append("| " + " | ".join(row) + " |")
    out += ["", "| tool | cost | time | helper scripts | timed out |", "| --- | --- | --- | --- | --- |"]
    for tool in tools:
        rs = [r for r in results if r["tool"] == tool]
        out.append(f"| {tool} | ${sum(r.get('cost_usd') or 0 for r in rs):.2f} | "
                   f"{sum(r['seconds'] for r in rs) / 60:.0f} min | {sum(r['helper_scripts'] for r in rs)} | "
                   f"{sum(r.get('timed_out', False) for r in rs)} |")
    out += ["", "Failed tasks, per tool:", ""]
    for tool in tools:
        bad = [r for r in results if r["tool"] == tool and not r["ok"]]
        out.append(f"- **{tool}** ({len(bad)}): " + ", ".join(f"`{r['task']}`" for r in bad))
    return "\n".join(out)


def friction(results, tool):
    notes = [(r["task"], r["friction"]) for r in results
             if r["tool"] == tool and r.get("friction") and r["friction"].lower().strip(" .") != "none"]
    return "\n".join(f"- `{task}`: {note}" for task, note in notes)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scripted", nargs="*", default=[])
    ap.add_argument("--agents", nargs="*", default=[])
    ap.add_argument("--friction", default="")
    args = ap.parse_args()
    for path in args.scripted:
        print(scripted_table(load(path), path), "\n")
    for path in args.agents:
        print(agent_table(load(path), path), "\n")
        if args.friction:
            print(f"#### Friction the {args.friction} agents named\n")
            print(friction(load(path), args.friction), "\n")


if __name__ == "__main__":
    main()
