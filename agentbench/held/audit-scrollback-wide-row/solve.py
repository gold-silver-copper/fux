"""Values read back from thousands of scrolled rows, one of them wider than
a usual terminal; then a counter's value once it is frozen."""
import re


def solve(d, work, fixtures):
    # Wide enough that row 0779 is one line.
    p = d.spawn(["python3", "-I", f"{fixtures}/audit_view.py", work], size=(400, 40))
    d.wait_text(p, "LIVE MONITOR", timeout=30)
    rows = d.screen(p, history=True)
    archive = re.findall(r"^ROW 0041 SEALED ARCHIVE = ([0-9a-f]+)", rows, re.M)[-1]
    wide = re.findall(r"^ROW 0779 WIDE INDEX .* CHECKSUM=([0-9a-f]+)", rows, re.M)[-1]
    d.send_and_wait_text(p, ["text:freeze", "Enter"], "FROZEN queue=")
    status = d.wait_exit(p)
    assert status == 0, f"the reader exited {status}"
    queue = re.findall(r"FROZEN queue=(\d+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"archive={archive}\nwide={wide}\nqueue={queue}\n")
