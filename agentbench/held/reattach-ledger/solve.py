"""A program left running while a terminal client comes and goes, found
again by its title."""
import re


def token(d, p, label):
    return re.findall(label + r" CHECKPOINT token=(\S+)", d.screen(p, history=True))[-1]


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/ledger_watch.py", work])
    d.wait_text(p, "mark>")
    opening = token(d, p, "OPENING")
    d.send_and_wait_text(p, [f"text:mark {opening}", "Enter"], "heartbeat=1")
    d.reattach()
    p = d.find_title("drift-ledger")
    d.wait_text(p, "SETTLED CHECKPOINT", timeout=20)
    settled = token(d, p, "SETTLED")
    d.reattach()
    d.send_and_wait_text(p, [f"text:mark {settled}", "Enter"], "MARKS VERIFIED")
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"opening={opening}\nsettled={settled}\n")
