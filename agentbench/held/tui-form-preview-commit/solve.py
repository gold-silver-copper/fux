"""A full-screen form filled with keys, its preview's fingerprint read and
typed back into the commit modal, then q."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/console.py", work], size=(90, 28))
    d.wait_text(p, "Environment: local", timeout=15)
    d.send_and_wait_text(p, ["Down", "Tab", "Right", "Tab", "C-u", "text:4", "Tab", "C-u",
                             "text:ready", "F2"], "Enter: close preview")
    fingerprint = re.findall(r"Fingerprint: (FP-[0-9A-F]+)", d.screen(p))[-1]
    d.send_and_wait_text(p, ["Enter", "F4"], "Type preview fingerprint")
    d.send_and_wait_text(p, [f"text:{fingerprint}", "Enter"], "Press q to quit cleanly")
    d.keys(p, "q")
    status = d.wait_exit(p)
    assert status == 0, f"the console exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"fingerprint={fingerprint}\n")
