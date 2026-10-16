#!/usr/bin/env python3
"""Second, blind judging by Codex (gpt-6.1-sol) of the runs the plan flags:
every grade the first judge changed from the agent's, and every hack,
worked_polling, impossible and invalid. One Codex run per tool, with the
frozen review prompt; it never sees the first judge's verdicts.

  codex_second.py SET [SIZE] [WORKERS]   (reads SET/second-needed.json: [[tool, run], ...])
"""
import concurrent.futures as cf
import json
import os
import subprocess
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from judges import TOOLDESC, DOCS, SRC  # noqa: E402

set_name = sys.argv[1]
need = json.load(open(f"{H}/{set_name}/second-needed.json"))
tmpl = open(f"{HERE}/review-prompt.md").read()


SIZE = int(sys.argv[2]) if len(sys.argv) > 2 else 15


def jobs():
    for tool in ("fux", "tmux", "zellij", "herdr"):
        names = sorted({r for t, r in need if t == tool})
        for i in range(0, len(names), SIZE):
            yield tool, i // SIZE, names[i:i + SIZE]


def run(job):
    tool, i, names = job
    out = f"{H}/{set_name}/second-{tool}"
    if os.path.exists(f"{out}/batch-{i:02}.json"):
        return tool, i, 0
    os.makedirs(f"{out}/lists", exist_ok=True)
    open(f"{out}/lists/batch-{i:02}.txt", "w").write("".join(f"{H}/{set_name}/runs/{tool}/{n}\n" for n in names))
    specs = f"{H}/{set_name}/specs"
    p = (tmpl.replace("`RULINGS`", f"{HERE}/rulings.md").replace("`TOOLDESC`", TOOLDESC[tool])
         .replace("`DOCS`", DOCS[tool]).replace("`SRC`", SRC[tool])
         .replace("`LIST`", f"{out}/lists/batch-{i:02}.txt").replace("`SPECS`", specs)
         .replace("SPECS/", specs + "/").replace("`OUT/BATCH.json`", f"{out}/batch-{i:02}.json").replace("`OUT`", out))
    p += ("\n\nAddition for this run (required by the analysis plan; not part of the frozen text above): you are a "
          "second, blind judge. Do not open any folder named review-* or second-* other than your own output "
          "folder, and do not look for anyone else's verdict.\n")
    open(f"{out}/prompt-{i:02}.md", "w").write(p)
    with open(f"{out}/codex-{i:02}.log", "w") as log:
        subprocess.run(["codex", "exec", "-m", "gpt-6.1-sol", "-c", 'service_tier="default"',
                        "-c", 'model_reasoning_effort="high"', "-s", "workspace-write", "--skip-git-repo-check",
                        "-C", out, "-o", f"{out}/summary-{i:02}.md", "-"],
                       stdin=open(f"{out}/prompt-{i:02}.md"), stdout=log, stderr=subprocess.STDOUT)
    return tool, i, len(names)


with cf.ThreadPoolExecutor(int(sys.argv[3]) if len(sys.argv) > 3 else 6) as ex:
    for tool, i, n in ex.map(run, list(jobs())):
        print(tool, i, n, "runs", flush=True)
