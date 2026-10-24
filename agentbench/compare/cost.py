#!/usr/bin/env python3
"""Cost to the agent, per run, from its call log and result (bench-161, part B/D).

Per run: calls (all fux calls), help (help, help-agents, explain), actions (the
rest), captures, waits (calls with wait-text, wait-gone, wait-exit or a bare
wait), repeated waits (a wait for the same text on the same target asked more
than once), restarts (respawn, or kill then run again of a named pane), sleeps,
seconds, tokens.

  cost.py RUN_DIR...            -> one JSON line per run

The call logs it reads are of the runs bench-161 recorded, through the fux
of that time (its wait words, statuses 3 and 7); today's fux has one wait,
`wait`, which WAIT counts too.
"""
import json, os, re, sys


def run_cost(d):
    calls = []
    try:
        for line in open(os.path.join(d, ".calls.log"), errors="replace"):
            parts = line.rstrip("\n").split("\t")
            if len(parts) >= 3 and parts[1] == "fux":
                calls.append(parts[2])
    except OSError:
        pass
    r = {}
    try:
        r = json.load(open(os.path.join(d, ".result.json")))
    except (OSError, json.JSONDecodeError):
        pass
    help_ = [c for c in calls if re.search(r"(^|\s)(help|help-agents|help-json)$|\bexplain$", c)]
    actions = [c for c in calls if c not in help_]
    waits = [c for c in actions if re.search(r"\bwait-(text|gone|exit|bell)\b|\bwait$|\bwait json$", c)]
    texts = []
    for c in waits:
        target = (re.match(r"\s*(%\d+|\S+)", c) or [None, ""])[1] if c.startswith("%") else ""
        for t in re.findall(r"str (\S+(?: \S+)*?) wait-(?:text|gone)", c):
            texts.append((target, t))
    repeated = sum(n - 1 for n in (texts.count(x) for x in set(texts)) if n > 1)
    return {
        "run": os.path.basename(d.rstrip("/")), "id": r.get("id"), "bin": r.get("bin"), "ok": r.get("ok"),
        "calls": len(calls), "help": len(help_), "actions": len(actions),
        "captures": sum("capture" in c for c in actions), "waits": len(waits), "repeated_waits": repeated,
        "restarts": sum(bool(re.search(r"\brespawn\b", c)) for c in actions),
        "sleeps": r.get("sleeps", 0), "seconds": r.get("seconds"), "tokens": r.get("tokens"),
    }


if __name__ == "__main__":
    for d in sys.argv[1:]:
        print(json.dumps(run_cost(d)))
