#!/usr/bin/env python3
"""The run list: every task × repeat × tool, shuffled with a fixed seed so
the tools are interleaved and machine load falls on all four alike.

  jobs.py SET REPEATS TASKS... > SET/jobs.json    (TASKS: ids, or "all")
"""
import json
import os
import random
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
TOOLS = ["fux", "tmux", "zellij", "herdr"]

set_name, repeats = sys.argv[1], int(sys.argv[2])
specs = os.path.join(H, set_name, "specs")
ids = sorted(os.listdir(specs)) if sys.argv[3:] == ["all"] else sys.argv[3:]
jobs = [[tool, os.path.join(H, "bins", "fux"), os.path.join(specs, t),
         os.path.join(H, set_name, "runs", tool, f"{t}-r{r}")]
        for t in ids for r in range(1, repeats + 1) for tool in TOOLS]
random.Random(4).shuffle(jobs)
print(json.dumps(jobs, indent=0))
