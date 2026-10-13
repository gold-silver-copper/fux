"""A row thousands of screen rows back, wrapped at 64 columns, and a
balance rewritten in place until it is final."""
import re


def unwrap(text, width):
    """Logical lines again: a row filled to the width goes on in the next."""
    out = []
    for row in text.split("\n"):
        if out and len(out[-1][-1]) >= width:
            out[-1].append(row)
        else:
            out.append([row])
    return ["".join(parts) for parts in out]


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/archive_reader.py", work], size=(64, 18))
    d.wait_text(p, "export>", timeout=30)
    rows = unwrap(d.screen(p, history=True), 64)
    archive, checksum = [re.search(r"archive=(\S+) checksum=(\S+) ENDROW", r).groups()
                         for r in rows if r.startswith("ROW 0147 ")][-1]
    balance = [r.split("BALANCE FINAL value=")[1].strip() for r in rows if "BALANCE FINAL value=" in r][-1]
    footers = [r.rstrip() for r in rows if r.startswith("footer ")][-3:]
    d.text(p, f"export {archive} {checksum} {balance}")
    d.keys(p, "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the reader exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"archive={archive}\nchecksum={checksum}\nbalance={balance}\n" + "".join(l + "\n" for l in footers))
