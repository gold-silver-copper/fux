#!/usr/bin/env python3
"""The normalisation pass: every run the first judges graded hack, for every
tool, re-read under the clarified rulings and the corrected capability table.
Writes one prompt per batch of SIZE runs per tool.

  normalize.py SET SIZE      -> SET/normal-<tool>/prompt-NN.md
"""
import glob
import json
import os
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from judges import TOOLDESC, DOCS  # noqa: E402

set_name, size = sys.argv[1], int(sys.argv[2])
tmpl = open(os.path.join(HERE, "normalize-prompt.md")).read()
specs = os.path.join(H, set_name, "specs")
for tool in ("fux", "tmux", "zellij", "herdr"):
    prior = {}
    for f in sorted(glob.glob(os.path.join(H, set_name, f"review-{tool}", "batch-*.json"))):
        for k, v in json.load(open(f)).items():
            prior[os.path.basename(k.rstrip("/"))] = v
    hacks = sorted(k for k, v in prior.items() if v.get("grade") == "hack")
    out = os.path.join(H, set_name, f"normal-{tool}")
    os.makedirs(os.path.join(out, "lists"), exist_ok=True)
    json.dump({k: prior[k] for k in hacks}, open(os.path.join(out, "prior.json"), "w"), indent=1)
    for b in range(0, len(hacks), size):
        i = b // size
        lst = os.path.join(out, "lists", f"batch-{i:02}.txt")
        open(lst, "w").write("".join(os.path.join(H, set_name, "runs", tool, k) + "\n" for k in hacks[b:b + size]))
        p = (tmpl.replace("`RULINGS`", os.path.join(HERE, "rulings.md"))
             .replace("`CAPS`", os.path.join(os.path.dirname(HERE), "writers", "caps.json")).replace("`TOOLDESC`", TOOLDESC[tool])
             .replace("`DOCS`", DOCS[tool]).replace("`LIST`", lst).replace("`SPECS`", specs)
             .replace("SPECS/", specs + "/").replace("`PRIOR`", os.path.join(out, "prior.json"))
             .replace("`OUT/BATCH.json`", os.path.join(out, f"batch-{i:02}.json")).replace("`OUT`", out))
        open(os.path.join(out, f"prompt-{i:02}.md"), "w").write(p)
    print(tool, len(hacks), "hack runs in", -(-len(hacks) // size), "batches ->", out)
