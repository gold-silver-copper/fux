"""Copies a spec folder: exactly its spec.json, check.py, solve_tmux.sh and
fixtures/ (writers leave scratch folders beside them)."""
import os
import shutil

KEEP = ("spec.json", "check.py", "solve_tmux.sh")


def copy(src, dst):
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst)
    for f in KEEP:
        if os.path.exists(os.path.join(src, f)):
            shutil.copy2(os.path.join(src, f), os.path.join(dst, f))
    if os.path.isdir(os.path.join(src, "fixtures")):
        shutil.copytree(os.path.join(src, "fixtures"), os.path.join(dst, "fixtures"),
                        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
