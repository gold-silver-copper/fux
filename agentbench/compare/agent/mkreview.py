#!/usr/bin/env python3
"""Opus review batches over finished agent runs.

  mkreview.py SET RUNS_DIR TOOLDESC DOCS SRC OUT [SIZE]
Runs already listed in OUT/lists are skipped; only finished runs are taken.
"""
import glob, os, sys
H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
set_name, runs, tooldesc, docs, src, out = sys.argv[1:7]
size = int(sys.argv[7]) if len(sys.argv) > 7 else 25
os.makedirs(os.path.join(out, "lists"), exist_ok=True)
listed = {l for f in glob.glob(os.path.join(out, "lists", "*.txt")) for l in open(f).read().split()}
first = len(glob.glob(os.path.join(out, "lists", "*.txt")))
done = sorted(d for d in glob.glob(os.path.join(H, set_name, runs, "*"))
              if os.path.exists(os.path.join(d, ".result.json")) and d not in listed)
tmpl = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "review-prompt.md")).read()
for b in range(0, len(done), size):
    i = first + b // size
    lst = os.path.join(out, "lists", f"batch-{i:02}.txt")
    open(lst, "w").write("".join(d + "\n" for d in done[b:b + size]))
    p = (tmpl.replace("`RULINGS`", os.path.join(os.path.dirname(os.path.abspath(__file__)), "rulings.md")).replace("`TOOLDESC`", tooldesc)
         .replace("`DOCS`", docs).replace("`SRC`", src).replace("`LIST`", lst)
         .replace("`SPECS`", os.path.join(H, set_name, "specs")).replace("SPECS/", os.path.join(H, set_name, "specs") + "/")
         .replace("`OUT/BATCH.json`", os.path.join(out, f"batch-{i:02}.json")).replace("`OUT`", out))
    open(os.path.join(out, f"prompt-{i:02}.md"), "w").write(p)
print(len(done), "runs in", -(-len(done) // size), "batches ->", out)
