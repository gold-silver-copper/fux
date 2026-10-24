"""Waiting for a text in fux, as `fux help agents recipes` does: shared by
speed.py and mem.py."""
import subprocess
import time


def run(bin_, e, *args):
    return subprocess.run([bin_, *args], env=e, capture_output=True, text=True, timeout=300)


def until_text(bin_, e, answer, text, secs=30):
    """From an action's answer (`%2 VERSION`), waits for each change until
    the pane's history holds `text`, `secs` at most in all: the pane. A
    timeout, a closed pane, a failure or the program's end first fails."""
    pane, version = answer.split()[:2]
    end = time.monotonic() + secs
    while text not in run(bin_, e, pane, "all-history", "capture").stdout:
        left = int(end - time.monotonic())
        if left < 1 or version.rsplit(".", 1)[-1] == "1":
            raise RuntimeError(f"{pane} never showed {text!r}, at {version}")
        out = run(bin_, e, pane, "str", version, "changed", str(left), "timeout", "wait")
        if out.returncode != 0:
            raise RuntimeError(f"{pane}: {out.returncode} {out.stderr.strip()}")
        version = out.stdout.split()[1]
    return pane
