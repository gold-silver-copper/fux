"""A row far back in a coloured archive, wrapped at 72 columns, then a
footer rewritten in place until it is final."""
import re


def unwrap(text, width):
    """Logical lines again: a row filled exactly to the width goes on in the
    next (a terminal that cuts rows at its width; fux's capture joins them
    already, its lines longer)."""
    out = []
    for row in text.split("\n"):
        if out and len(out[-1][-1]) == width:
            out[-1].append(row)
        else:
            out.append([row])
    return ["".join(parts) for parts in out]


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/ledger.py", work], size=(72, 28))
    d.wait_text(p, "Archive request>", timeout=20)
    rows = unwrap(d.screen(p, history=True), 72)
    head = "ROW 0047 AUDIT ANCHOR: "
    anchor = [r[len(head):].rstrip() for r in rows if r.startswith(head)][-1]
    d.send_and_wait_text(p, ["text:show tail", "Enter"], "Footer receipt>", timeout=20)
    screen = d.screen(p, history=True)
    final = re.findall(r"Footer FINAL value=(\S+)", screen)[-1]
    tails = re.findall(r"^TAIL (\S+)$", screen, re.M)[-3:]
    d.text(p, "accept " + final)
    d.keys(p, "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the ledger exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"{anchor}\n{final}\n{','.join(tails)}\n")
