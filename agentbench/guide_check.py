#!/usr/bin/env python3
"""Check that the agents' guide names the native/library words the driver uses.

    python3 -I agentbench/guide_check.py --fux-bin target/release/fux

The vocabulary comes from that binary's public help, not Rust source syntax.
"""
import argparse
import ast
from pathlib import Path
import re
import subprocess

HERE = Path(__file__).resolve().parent
# Printed by fux, each name and where it comes from.
DICTIONARY = ('(for-each (lambda! (w) (print (string-append (get w "name") " " '
              '(get w "provenance")))) (dictionary))')
# The harness's own, which is no word the guide need name.
OWN = {"__serve"}


def words_of_fux(dictionary):
    """fux's own and its libraries' names, from the lines `NAME PROVENANCE`
    `DICTIONARY` prints."""
    rows = [line.split() for line in dictionary.splitlines()]
    known = {name for name, provenance in rows if provenance in ("fux", "library")}
    if not known:
        raise ValueError("fux's dictionary listed no fux or library words")
    return known


def driver_words(source, known):
    driver = next(node for node in ast.parse(source).body
                  if isinstance(node, ast.ClassDef) and node.name == "Fux")
    # Ignore field lookups: operators are what follows a parenthesis in a
    # program's text.
    fields = {id(child) for node in ast.walk(driver) if isinstance(node, ast.Subscript)
              for child in ast.walk(node.slice)}
    quoted = {word for node in ast.walk(driver)
              if isinstance(node, ast.Constant) and isinstance(node.value, str)
              and id(node) not in fields
              for word in re.findall(r"\(([^\s()\"]+)", node.value)}
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

    def fux(program):
        return subprocess.run([args.fux_bin, program], check=True,
                              capture_output=True, text=True, timeout=10).stdout

    known = words_of_fux(fux(DICTIONARY))
    used = driver_words((HERE / "drivers.py").read_text(), known)
    missing = missing_words(used, fux('(explain "agents")'))
    if missing:
        print("the full guide does not name:", " ".join(missing))
        return 1
    print(f"the full guide names all {len(used)} words the driver uses "
          f"({len(known)} native/library words published)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
