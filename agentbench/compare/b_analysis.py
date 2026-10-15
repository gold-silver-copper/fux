#!/usr/bin/env python3
"""Part B: set D, builds x and y (blind until --unmask), 3 repeats per task.
Clean = graded worked by the frozen judge (D/review-x, D/review-y); a task is
dropped from both builds if any judge ruled its spec invalid. First try =
checker passed and no restart. Majority per task (2 of 3) for McNemar;
Fisher on all runs; Wilcoxon on per-task means of each cost measure."""
import glob, json, os, statistics, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cost import run_cost
from stats import wilson, fisher, mcnemar, wilcoxon
H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
MEAS = ["calls", "actions", "waits", "captures", "repeated_waits", "restarts", "seconds", "tokens"]


def grades(b):
    g = {}
    for f in glob.glob(f"{H}/D/review-{b}/batch-*.json"):
        for k, x in json.load(open(f)).items():
            g[k.split("/")[-1]] = x
    for f in glob.glob(f"{H}/D/second-{b}/batch-*.json"):  # second judges, reported apart
        pass
    return g


def load(b):
    G = grades(b)
    runs = {}
    for d in sorted(glob.glob(f"{H}/D/runs-{b}/TD-*-r*")):
        name = os.path.basename(d)
        if "." in name or not os.path.exists(os.path.join(d, ".result.json")):
            continue  # the fixtures copied beside a run, not a run
        c = run_cost(d)
        c["id"] = c["id"] or name.rsplit("-r", 1)[0]
        g = G.get(name, {})
        c["grade"] = g.get("grade")
        c["invalid"] = g.get("grade") == "invalid" or g.get("valid") is False
        c["clean"] = g.get("grade") == "worked"
        c["first_try"] = bool(c["ok"]) and c["restarts"] == 0
        runs[name] = c
    return runs


R = {b: load(b) for b in ("x", "y")}
label = {"x": "x", "y": "y"}
if "--unmask" in sys.argv:
    m = dict(kv.split("=") for kv in open(os.path.join(H, "blind-map.txt")).read().split())
    label = {"x": "master" if m["x"] == "base" else "#161", "y": "master" if m["y"] == "base" else "#161"}
invalid = {r["id"] for b in R for r in R[b].values() if r["invalid"]}
tasks = sorted({r["id"] for b in R for r in R[b].values()} - invalid)
seed_of = {}
for t in tasks:
    n = int(t.split("-")[1]); seed_of[t] = "docs" if n <= 15 else "issues"
print(f"invalid tasks dropped from both: {sorted(invalid)}; tasks kept: {len(tasks)}")
for half in ("all", "docs", "issues"):
    ts = [t for t in tasks if half == "all" or seed_of[t] == half]
    print(f"\n## {half}: {len(ts)} tasks")
    per = {}
    for b in R:
        runs = [r for r in R[b].values() if r["id"] in ts]
        k = sum(r["clean"] for r in runs); n = len(runs); ft = sum(r["first_try"] for r in runs)
        lo, hi = wilson(k, n); flo, fhi = wilson(ft, n)
        per[b] = (k, n, ft)
        print(f"{label[b]:7} clean {k}/{n} = {100*k/max(n,1):.1f}% ({lo:.1f}-{hi:.1f}); first try {ft}/{n} = {100*ft/max(n,1):.1f}% ({flo:.1f}-{fhi:.1f})")
    (kx, nx, fx), (ky, ny, fy) = per["x"], per["y"]
    print(f"Fisher all runs: clean p={fisher(kx, nx-kx, ky, ny-ky):.3g}; first try p={fisher(fx, nx-fx, fy, ny-fy):.3g}")
    maj = lambda b, t: sum(r["clean"] for r in R[b].values() if r["id"] == t) >= 2
    only_x = sum(maj("x", t) and not maj("y", t) for t in ts); only_y = sum(maj("y", t) and not maj("x", t) for t in ts)
    print(f"McNemar per task (majority clean): only {label['x']} {only_x}, only {label['y']} {only_y}, p={mcnemar(only_x, only_y):.3g}")
    for m in MEAS:
        mean = lambda b, t: statistics.mean((r[m] or 0) for r in R[b].values() if r["id"] == t)
        xs = [mean("x", t) for t in ts]; ys = [mean("y", t) for t in ts]
        p, med, n = wilcoxon(xs, ys)
        print(f"  {m:15} {label['x']} med {statistics.median(xs):9.1f} mean {statistics.mean(xs):9.1f} | {label['y']} med {statistics.median(ys):9.1f} mean {statistics.mean(ys):9.1f} | Wilcoxon p={p:.3g}")
