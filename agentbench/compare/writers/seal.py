#!/usr/bin/env python3
"""Seals a set: a SHA-256 per spec (every file in its folder, by path and
content) in a ledger, copied to the notes; then the specs folder is locked
with chmod 000. `check` unlocks and compares every hash.

  seal.py seal SET      seal.py check SET
"""
import hashlib
import os
import shutil
import sys
import time

H = os.environ.get("BENCH_DIR", os.path.expanduser("~/.cache/fux-bench"))
NOTES = os.environ.get("BENCH_NOTES", os.path.join(H, "notes"))


def digest(d):
    h = hashlib.sha256()
    for root, dirs, files in sorted(os.walk(d)):
        dirs.sort()
        for f in sorted(files):
            if f.endswith(".pyc") or "__pycache__" in root:
                continue
            p = os.path.join(root, f)
            h.update(os.path.relpath(p, d).encode() + b"\0" + open(p, "rb").read() + b"\0")
    return h.hexdigest()


def main():
    cmd, set_name = sys.argv[1:3]
    specs = os.path.join(H, set_name, "specs")
    ledger = os.path.join(H, set_name, "ledger.tsv")
    if cmd == "seal":
        rows = [f"{sid}\t{digest(os.path.join(specs, sid))}" for sid in sorted(os.listdir(specs))]
        open(ledger, "w").write("\n".join(rows) + "\n")
        os.makedirs(os.path.join(NOTES, set_name), exist_ok=True)
        shutil.copy(ledger, os.path.join(NOTES, set_name, "ledger.tsv"))
        with open(os.path.join(NOTES, set_name, "integrity.txt"), "a") as f:
            f.write(f"sealed {len(rows)} specs {time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n")
        os.chmod(specs, 0)
        print(f"sealed {len(rows)} specs; ledger {ledger}")
    else:
        os.chmod(specs, 0o755)
        want = dict(l.split("\t") for l in open(ledger).read().split("\n") if l)
        bad = [sid for sid, h in want.items() if digest(os.path.join(specs, sid)) != h]
        with open(os.path.join(NOTES, set_name, "integrity.txt"), "a") as f:
            f.write(f"checked {len(want) - len(bad)}/{len(want)} hashes match {time.strftime('%Y-%m-%dT%H:%M:%S%z')} {bad}\n")
        print(f"{len(want) - len(bad)}/{len(want)} hashes match {bad}")


if __name__ == "__main__":
    main()
