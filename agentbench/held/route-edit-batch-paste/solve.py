"""A password read off the screen, a prefilled route edited with Home,
Delete, End and Backspace, a file pasted whole, then Ctrl-D."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/batch_console.py", work])
    d.wait_text(p, "Password (hidden)>", timeout=15)
    password = re.findall(r"CURRENT PASSWORD: (\S+)", d.screen(p, history=True))[-1]
    d.send_and_wait_text(p, [f"text:{password}", "Enter"], "Route west-14")
    d.send_and_wait_text(p, ["Home", "Delete", "Delete", "Delete", "Delete", "text:east", "End",
                             "BSpace", "BSpace", "text:27", "F2"], "BATCH READY")
    with open(f"{work}/batch.txt", encoding="utf-8") as f:
        d.paste(p, f.read())
    d.send_and_wait_text(p, ["Enter"], "SAVE READY")
    d.keys(p, "C-d")
    status = d.wait_exit(p)
    assert status == 0, f"the console exited {status}"
    route, lines = re.findall(r"BATCH VALIDATED: route=(\S+) paste-lines=(\d+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"route={route}\npaste-lines={lines}\n")
