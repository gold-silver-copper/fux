#!/usr/bin/env python3
"""Part A: oracle bench, base vs candidate on the same cases.

Usage: a_oracle.py [--manifest FILE] [--write-manifest FILE] SEED...

Reads `oracle-base-SEED.json` and `oracle-cand-SEED.json` from $BENCH_DIR/A,
each written by its own build's harness (`ORACLE_BENCH`). A disagreement is
keyed by (seed, case, step, class), a case by its mode, seed and chunk, so a
disagreement of one class never stands in for another at the same step.

Whether a disagreement is known is decided here, by the KNOWN predicates
frozen from the base's tests/oracle.rs (class, classify source, fixture mode,
chunking), on both sides alike: a "known" label in a build's output is
ignored.

Exits 1 on a problem:
- a seed with no cases, a case missing from a build or there twice, a case
  the manifest lists that a build lacks (or one it doesn't list), a request
  left unanswered, a case whose step count changed;
- a disagreement the candidate has and the base has not, that no frozen
  KNOWN predicate covers;
- per class and in total, candidate-only disagreements outnumbering
  base-only ones by a one-sided exact McNemar test at p < 0.05, Holm-
  corrected across the classes.
A disagreement only the base has is listed as resolved. A known one on one
side alone is listed, not failed: the base against itself shows them as
timing."""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stats import holm
import math

# Frozen from tests/oracle.rs's KNOWN at 8fe55f1e: (class, sources, modes,
# chunked only). An empty list means any.
KNOWN = [("echo-taken-as-output", ["screen"], ["full"], False)]


def known(x, case):
    mode, _, chunk = case
    return any(cls == x["class"] and (not srcs or x["source"] in srcs)
               and (not modes or mode in modes) and (not chunked or chunk > 0)
               for cls, srcs, modes, chunked in KNOWN)


def one_sided(base_only, cand_only):
    """Exact one-sided McNemar: the chance of cand_only or more of n discordant
    pairs if each side were equally likely to have them."""
    n = base_only + cand_only
    if n == 0:
        return 1.0
    return min(1.0, sum(math.comb(n, i) for i in range(cand_only, n + 1)) / 2 ** n)


args = sys.argv[1:]


def flag(name):
    if name in args:
        k = args.index(name)
        value = args[k + 1]
        del args[k:k + 2]
        return value
    return None


manifest_in, manifest_out = flag("--manifest"), flag("--write-manifest")
S = os.path.join(os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench")), "A")
seeds = args or ["101", "202", "303"]
problems = []
steps = 0
dis = {"base": {}, "cand": {}}
all_cases = {}
for seed in seeds:
    cases = {}
    for b in ("base", "cand"):
        d = json.load(open(f"{S}/oracle-{b}-{seed}.json"))
        if d.get("unanswered", 0):
            problems.append(f"seed {seed}: {b} left {d['unanswered']} unanswered")
        if not d.get("cases"):
            problems.append(f"seed {seed}: {b} has no cases")
        cases[b] = {}
        for c in d.get("cases", []):
            case = (c["mode"], c["seed"], c["chunk"])
            if case in cases[b]:
                problems.append(f"seed {seed}: {b} has case {case} twice")
            cases[b][case] = c
            for x in c["disagreements"]:
                dis[b][(seed, case, x["step"], x["class"])] = known(x, case)
    for case in cases["base"].keys() ^ cases["cand"].keys():
        problems.append(f"seed {seed}: case {case} is in one build only")
    for case in cases["base"].keys() & cases["cand"].keys():
        base, cand = cases["base"][case]["steps"], cases["cand"][case]["steps"]
        steps += base
        if base != cand:
            problems.append(f"seed {seed}: case {case} took {base} steps on base, {cand} on cand")
    all_cases[seed] = sorted(cases["base"].keys() | cases["cand"].keys())

if manifest_out:
    with open(manifest_out, "w") as f:
        json.dump({s: [list(c) for c in cs] for s, cs in all_cases.items()}, f, indent=1)
if manifest_in:
    want = json.load(open(manifest_in))
    for seed in seeds:
        if seed not in want:
            problems.append(f"seed {seed}: not in the manifest")
            continue
        listed = {tuple(c) for c in want[seed]}
        for case in listed ^ set(all_cases[seed]):
            problems.append(f"seed {seed}: case {case} differs from the manifest")
elif not manifest_out:
    problems.append("no --manifest given: the case list can't be checked")

classes = sorted({k[3] for b in dis for k in dis[b]})
print(f"oracle: seeds {seeds}, {steps} checks per build")
print(f"{'class':28} {'base':>14} {'cand':>14} {'only base':>12} {'only cand':>10} {'p (1-sided)':>12}")
rate = lambda n: 100 * n / max(steps, 1)
ps = []
for cl in classes:
    a = {k for k in dis["base"] if k[3] == cl}
    c = {k for k in dis["cand"] if k[3] == cl}
    p = one_sided(len(a - c), len(c - a))
    ps.append(p)
    print(f"{cl:28} {len(a):5} ({rate(len(a)):4.1f}%) {len(c):5} ({rate(len(c)):4.1f}%) {len(a-c):12} {len(c-a):10} {p:12.2g}")
for cl, p, adj in zip(classes, ps, holm(ps)):
    if adj < 0.05:
        problems.append(f"class {cl}: more disagreements on cand than base (one-sided McNemar p {p:.2g}, Holm {adj:.2g})")
a, c = set(dis["base"]), set(dis["cand"])
p = one_sided(len(a - c), len(c - a))
print(f"{'ALL':28} {len(a):5} ({rate(len(a)):4.1f}%) {len(c):5} ({rate(len(c)):4.1f}%) {len(a-c):12} {len(c-a):10} {p:12.2g}")
if p < 0.05:
    problems.append(f"more disagreements on cand than base in total (one-sided McNemar p {p:.2g})")
for k in sorted(c - a):
    what = f"seed {k[0]}, case {k[1]}, step {k[2]}, class {k[3]}"
    if dis["cand"][k]:
        print(f"one-sided on cand, known (timing): {what}")
    else:
        problems.append(f"new disagreement: {what}")
for k in sorted(a - c):
    print(f"resolved (base only{', known' if dis['base'][k] else ''}): seed {k[0]}, case {k[1]}, step {k[2]}, class {k[3]}")
for problem in problems:
    print("problem:", problem)
sys.exit(1 if problems else 0)
