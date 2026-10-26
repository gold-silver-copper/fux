#!/usr/bin/env python3
"""What the judges are told about each tool, and the first judges' review
batches: SIZE runs each, one tool per batch.

  judges.py SET SIZE      -> SET/review-<tool>/prompt-NN.md
"""
import glob
import os
import sys

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
CAPS = f"{H}/caps"
TOOLDESC = {
    "fux": "fux, master 9f8a7804 (the agent called it directly inside its sandbox)",
    "tmux": "tmux 3.7c (the agent's calls went through a proxy outside its sandbox, which streams output back)",
    "zellij": "zellij 0.45.1 (the agent's calls went through a proxy outside its sandbox, which streams output back)",
    "herdr": "herdr 0.9.1 (the agent's calls went through a proxy outside its sandbox, which streams output back)",
}
DOCS = {"fux": f"{CAPS}/fux-docs.txt (fux help agents and fux help)",
        "tmux": f"{CAPS}/tmux-man.txt (man tmux)",
        "zellij": f"{CAPS}/zellij-docs.txt (zellij --help, zellij action --help and each action's help)",
        "herdr": f"{CAPS}/herdr-docs.txt (herdr --help, herdr pane, herdr --skill)"}
SRC = {"fux": f"{H}/src-fux", "tmux": "not available: judge from the man page",
       "zellij": "not available: judge from its help", "herdr": "not available: judge from its help and skill"}


def main():
    set_name, size = sys.argv[1], int(sys.argv[2])
    here = os.path.dirname(os.path.abspath(__file__))
    tmpl = open(os.path.join(here, "review-prompt.md")).read()
    specs = os.path.join(H, set_name, "specs")
    for tool in ("fux", "tmux", "zellij", "herdr"):
        out = os.path.join(H, set_name, f"review-{tool}")
        os.makedirs(os.path.join(out, "lists"), exist_ok=True)
        listed = {l for f in glob.glob(os.path.join(out, "lists", "*.txt")) for l in open(f).read().split()}
        first = len(glob.glob(os.path.join(out, "lists", "*.txt")))
        done = sorted(d for d in glob.glob(os.path.join(H, set_name, "runs", tool, "*"))
                      if os.path.exists(os.path.join(d, ".result.json")) and d not in listed)
        for b in range(0, len(done), size):
            i = first + b // size
            lst = os.path.join(out, "lists", f"batch-{i:02}.txt")
            open(lst, "w").write("".join(d + "\n" for d in done[b:b + size]))
            p = (tmpl.replace("`RULINGS`", os.path.join(here, "rulings.md"))
                 .replace("`TOOLDESC`", TOOLDESC[tool]).replace("`DOCS`", DOCS[tool]).replace("`SRC`", SRC[tool])
                 .replace("`LIST`", lst).replace("`SPECS`", specs).replace("SPECS/", specs + "/")
                 .replace("`OUT/BATCH.json`", os.path.join(out, f"batch-{i:02}.json")).replace("`OUT`", out))
            open(os.path.join(out, f"prompt-{i:02}.md"), "w").write(p)
        print(tool, len(done), "runs in", -(-len(done) // size), "batches ->", out)


if __name__ == "__main__":
    main()
