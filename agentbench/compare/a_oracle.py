#!/usr/bin/env python3
"""Part A: oracle bench, base vs candidate on the same cases. Per class:
disagreements and their rate; paired by (seed, case, step), a case known
by its mode, seed and chunk, as results arrive in the order they finish:
McNemar on steps where exactly one build disagreed. Exits 1 on a problem: a
case missing from a build or there twice, a request left unanswered, a case
whose step count changed, or a disagreement the candidate has and the base
has not."""
import json, os, sys, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stats import mcnemar
S = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "A")
seeds = sys.argv[1:] or ["101", "202", "303"]
problems = []
steps = 0; dis = {"base": {}, "cand": {}}
for seed in seeds:
    cases = {}
    for b in ("base", "cand"):
        d = json.load(open(f"{S}/oracle-{b}-{seed}.json"))
        if d.get("unanswered", 0):
            problems.append(f"seed {seed}: {b} left {d['unanswered']} unanswered")
        cases[b] = {}
        for c in d["cases"]:
            case = (c["mode"], c["seed"], c["chunk"])
            if case in cases[b]:
                problems.append(f"seed {seed}: {b} has case {case} twice")
            cases[b][case] = c
            for x in c["disagreements"]:
                dis[b][(seed, case, x["step"], x["class"])] = x
    for case in cases["base"].keys() ^ cases["cand"].keys():
        problems.append(f"seed {seed}: case {case} is in one build only")
    for case in cases["base"].keys() & cases["cand"].keys():
        base, cand = cases["base"][case]["steps"], cases["cand"][case]["steps"]
        steps += base
        if base != cand:
            problems.append(f"seed {seed}: case {case} took {base} steps on base, {cand} on cand")
classes = sorted({k[3] for b in dis for k in dis[b]})
print(f"oracle: seeds {seeds}, {steps} checks per build")
print(f"{'class':28} {'base':>14} {'cand':>14} {'only base':>12} {'only cand':>10} {'McNemar p':>10}")
for cl in classes + ["ALL"]:
    keys = lambda b: {k[:3] for k in dis[b] if cl == "ALL" or k[3] == cl}
    a, c = keys("base"), keys("cand")
    rate = lambda n: 100 * n / max(steps, 1)
    print(f"{cl:28} {len(a):5} ({rate(len(a)):4.1f}%) {len(c):5} ({rate(len(c)):4.1f}%) {len(a-c):12} {len(c-a):10} {mcnemar(len(a-c), len(c-a)):10.2g}")
    if cl == "ALL":
        for k in sorted(c - a):
            problems.append(f"new disagreement: seed {k[0]}, case {k[1]}, step {k[2]}")
known = collections.Counter((b, x["class"], x["known"]) for b in dis for x in dis[b].values())
print("known (kept-rule) split:", dict(known))
for problem in problems:
    print("problem:", problem)
sys.exit(1 if problems else 0)
