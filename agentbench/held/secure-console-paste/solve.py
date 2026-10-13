"""Arrows, Alt and function keys, a hidden password read off the screen, a
bracketed paste of a file, then Ctrl-D."""
import re
import shlex


def solve(d, work, fixtures):
    p = d.spawn(shlex.split(f"python3 -I {fixtures}/secure_console.py") + [work])
    d.wait_text(p, "TARGET:")
    d.send_and_wait_text(p, ["Right", "Enter"], "SHORTCUT:")
    d.send_and_wait_text(p, ["M-v"], "TOOLS:")
    d.send_and_wait_text(p, ["F2"], "PASSWORD")
    password = re.findall(r"PASSWORD \(hidden; enter ([^)]*)\)", d.screen(p))[-1]
    d.send_and_wait_text(p, ["text:" + password, "Enter"], "PASTE NOTE now")
    with open(f"{work}/release-note.txt", encoding="utf-8") as f:
        d.paste(p, f.read())
    d.wait_text(p, "SUBMIT with Ctrl-D")
    d.keys(p, "C-d")
    status = d.wait_exit(p)
    assert status == 0, f"the console exited {status}"
    count = re.findall(r"Note accepted; bytes=(\d+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"note=accepted\nbytes={count}\n")
