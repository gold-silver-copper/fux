"""Waiting for a text in fux, as the agents' guide does: shared by
speed.py and mem.py."""
import subprocess
import time


def string(text):
    """`text` as a grift string."""
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def agent_tab(argv, history=None):
    """The program that starts `argv` in a tab of workspace agent."""
    options = '(size (in (options) "agent") 80 24)'
    if history is not None:
        options = f"(history-bytes {options} {history})"
    options = f"(program {options} {string(argv[0])})"
    for word in argv[1:]:
        options = f"(argument {options} {string(word)})"
    return f"(new-tab {options})"


def run(bin_, e, *args):
    return subprocess.run([bin_, *args], env=e, capture_output=True, text=True, timeout=300)


def until_text(bin_, e, answer, text, secs=30):
    """From an action's answer (`%2:0.0.0.0.0:RUN`, its pane's version),
    waits for each change until the pane's history holds `text`, `secs` at
    most in all: the pane. A timeout, a closed pane, a failure or the
    program's end first fails."""
    version = answer.split()[0]
    pane = version.split(":")[0]
    end = time.monotonic() + secs
    while text not in run(bin_, e, f"(capture (all-history (pane {string(pane)})))").stdout:
        left = int(end - time.monotonic())
        if left < 1 or version.split(":")[1].endswith(".1"):
            raise RuntimeError(f"{pane} never showed {text!r}, at {version}")
        out = run(bin_, e, f"(wait (timeout (changed {string(version)}) {left}))")
        if out.returncode != 0:
            raise RuntimeError(f"{pane}: {out.returncode} {out.stderr.strip()}")
        version = out.stdout.split()[0]
    return pane
