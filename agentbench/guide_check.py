#!/usr/bin/env python3
"""Check that `fux help agents` names the native/library words the driver uses.

    python3 -I agentbench/guide_check.py --fux-bin target/release/fux

The vocabulary comes from that binary's public help, not Rust source syntax.
"""
import argparse
import ast
from pathlib import Path
import re
import subprocess

HERE = Path(__file__).resolve().parent
# The harness's own: starting and stopping a server out of the agent's way.
OWN = {"__serve", "force-kill-server"}


def words_of_fux(help_text):
    """Native rows and bundled library names in `fux help`."""
    native, separator, rest = help_text.partition("\nThe libraries (`fux help WORD` says each):\n")
    libraries, end, _ = rest.partition("\n\nThe server:")
    native = set(re.findall(r"^  (\S+)", native, re.M))
    libraries = set(libraries.split())
    if not separator or not end or not native or not libraries:
        raise ValueError("fux help did not list both native and library words")
    return native | libraries


def driver_words(source, known):
    driver = next(node for node in ast.parse(source).body
                  if isinstance(node, ast.ClassDef) and node.name == "Fux")
    # Ignore JSON field lookups, but keep single-item argument lists.
    fields = {id(child) for node in ast.walk(driver) if isinstance(node, ast.Subscript)
              for child in ast.walk(node.slice)}
    quoted = {node.value for node in ast.walk(driver)
              if isinstance(node, ast.Constant) and isinstance(node.value, str)
              and id(node) not in fields}
    used = quoted & known - OWN
    if not used:
        raise ValueError("the fux driver references no published words")
    return used


def missing_words(used, guide):
    return sorted(w for w in used if not re.search(
        r"(?<![\w-])" + re.escape(w) + r"(?![\w-])", guide))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--fux-bin", required=True)
    args = ap.parse_args()

    def help_for(*topic):
        return subprocess.run([args.fux_bin, "help", *topic], check=True,
                              capture_output=True, text=True, timeout=10).stdout

    known = words_of_fux(help_for())
    used = driver_words((HERE / "drivers.py").read_text(), known)
    missing = missing_words(used, help_for("agents"))
    if missing:
        print("the guide's first screen does not name:", " ".join(missing))
        return 1
    print(f"the guide's first screen names all {len(used)} words the driver uses "
          f"({len(known)} native/library words published)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
