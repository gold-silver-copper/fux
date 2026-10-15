#!/usr/bin/env python3
"""Part A: oracle bench, master vs #161 on the same cases. Per class:
disagreements and rate (Wilson); paired by (seed, case, step): McNemar on
steps where exactly one build disagreed."""
import json, os, sys, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stats import wilson, mcnemar
S = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "A")
seeds = sys.argv[1:] or ["101", "202", "303"]
steps = 0; dis = {"base": {}, "cand": {}}
for seed in seeds:
    for b in ("base", "cand"):
        d = json.load(open(f"{S}/oracle-{b}-{seed}.json"))
        for i, c in enumerate(d["cases"]):
            if b == "base":
                steps += c["steps"]
            for x in c["disagreements"]:
                dis[b][(seed, i, x["step"], x["class"])] = x
classes = sorted({k[3] for b in dis for k in dis[b]})
print(f"oracle: seeds {seeds}, {steps} checks per build")
print(f"{'class':28} {'master':>14} {'#161':>14} {'only master':>12} {'only #161':>10} {'McNemar p':>10}")
for cl in classes + ["ALL"]:
    keys = lambda b: {k[:3] for k in dis[b] if cl == "ALL" or k[3] == cl}
    a, c = keys("base"), keys("cand")
    lo, hi = wilson(len(a), steps); lo2, hi2 = wilson(len(c), steps)
    print(f"{cl:28} {len(a):5} ({100*len(a)/steps:4.1f}%) {len(c):5} ({100*len(c)/steps:4.1f}%) {len(a-c):12} {len(c-a):10} {mcnemar(len(a-c), len(c-a)):10.2g}")
known = collections.Counter((b, x["class"], x["known"]) for b in dis for x in dis[b].values())
print("known (kept-rule) split:", dict(known))
