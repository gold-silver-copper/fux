"""A full-screen notebook: a key found deep in its help pager, the exact
incident picked out of a filter, an edit redone after a stale save."""
import re


def edit(d, p, approval, until):
    """Severity two steps down to high, owner ivy, the approval key, save."""
    d.send_and_wait_text(p, ["Down", "Down", "Tab", "C-u", "text:ivy", "Tab", f"text:{approval}", "C-s"],
                         until)


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/notebook.py", work])
    d.wait_text(p, "Incident notebook --")
    d.send_and_wait_text(p, ["text:?"], "HELP PAGER")
    d.send_and_wait_text(p, ["PageDown", "PageDown"], "DEPENDENCY D-07 approval key=")
    approval = re.findall(r"DEPENDENCY D-07 approval key=(\w+)", d.screen(p))[-1]
    d.send_and_wait_text(p, ["text:q/"], "Filter input:")
    d.send_and_wait_text(p, ["text:Q-24", "Enter"], "Filter: Q-24")
    # Two incidents match; the one whose ID is exactly Q-24.
    rows = [l for l in d.screen(p).split("\n") if re.match(r"^[> ] \S+ \| ", l)]
    target = next(i for i, l in enumerate(rows) if l[2:].split(" | ")[0] == "Q-24")
    d.send_and_wait_text(p, ["Down"] * target + ["Enter"], "Incident Q-24")
    d.send_and_wait_text(p, ["text:e"], "EDIT Q-24 revision=")
    edit(d, p, approval, "STALE REVISION")
    d.send_and_wait_text(p, ["text:R"], "EDIT Q-24 revision=")
    revision = re.findall(r"EDIT Q-24 revision=(\d+)", d.screen(p))[-1]
    edit(d, p, approval, "WRITE CONFIRMATION")
    d.keys(p, "Right", "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the notebook exited {status}"
    receipt = re.findall(r"NOTEBOOK SAVED receipt=([0-9a-f]+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"ticket=Q-24\nseverity=high\nowner=ivy\nrevision={revision}\nreceipt={receipt}\n")
