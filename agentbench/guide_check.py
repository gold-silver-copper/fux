#!/usr/bin/env python3
"""Checks that the first screen of `fux help agents` (docs/agents.md, its
first 60 lines) names every fux word the benchmark's fux driver uses to do
its 100 tasks: what an agent needs should be on that screen.

    python3 -I agentbench/guide_check.py
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# The harness's own: starting and stopping a server out of the agent's way.
OWN = {"__serve", "force-kill-server"}


def words_of_fux():
    """Every word fux has: its table's, and the bundled library's."""
    root = os.path.join(HERE, "..", "src", "script")
    table = open(os.path.join(root, "words.rs")).read()
    library = open(os.path.join(root, "library.fux")).read()
    return set(re.findall(r'word\("[^"]+", "([a-z-]+)"', table)) | set(re.findall(r"^define (\S+)", library, re.M))


def main():
    source = open(os.path.join(HERE, "drivers.py")).read()
    driver = source[source.index("class Fux(Driver):"):source.index("# " + "-" * 70 + " tmux")]
    known = words_of_fux()
    # Words, not the JSON fields the driver reads (`s["cols"]`).
    quoted = set(re.findall(r'"([a-z][a-z-]*)"', driver)) - set(re.findall(r'\["([a-z_-]+)"\]', driver))
    used = {w for w in quoted if w in known and w not in OWN}
    guide = open(os.path.join(HERE, "..", "docs", "agents.md")).read().split("\n")
    head = "\n".join(guide[:60])
    missing = sorted(w for w in used if not re.search(r"(?<![\w-])" + re.escape(w) + r"(?![\w-])", head))
    if missing:
        print("the guide's first 60 lines do not name:", " ".join(missing))
        sys.exit(1)
    print(f"the guide's first 60 lines name all {len(used)} words the driver uses")


if __name__ == "__main__":
    main()
