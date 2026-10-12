#!/usr/bin/env python3
"""Checks that the first screen of `fux help agents` (docs/agents.md, its
first 60 lines) names every fux command and flag the benchmark's fux driver
uses to do its 100 tasks: what an agent needs should be on that screen.

    python3 -I agentbench/guide_check.py
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
COMMANDS = {"run", "wait", "send-keys", "send-mouse", "capture-pane", "resize", "resize-pane",
            "split", "kill-pane", "respawn", "ls"}
# The harness's own: starting a server, and a shell started bare on purpose.
OWN = {"--socket", "--norc", "-i"}


def main():
    source = open(os.path.join(HERE, "drivers.py")).read()
    driver = source[source.index("class Fux(Driver):"):source.index("# " + "-" * 70 + " tmux")]
    words = set(re.findall(r'"(--?[a-z][a-z-]*|[a-z]+(?:-[a-z]+)*)"', driver))
    used = {w for w in words if (w.startswith("-") or w in COMMANDS) and w not in OWN}
    guide = open(os.path.join(HERE, "..", "docs", "agents.md")).read().split("\n")
    head = "\n".join(guide[:60])
    missing = sorted(w for w in used if not re.search(r"(?<![\w-])" + re.escape(w) + r"(?![\w-])", head))
    if missing:
        print("the guide's first 60 lines do not name:", " ".join(missing))
        sys.exit(1)
    print(f"the guide's first 60 lines name all {len(used)} commands and flags the driver uses")


if __name__ == "__main__":
    main()
