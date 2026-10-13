"""A full-screen board: a list, a pager whose second page holds the code,
a form, a modal whose default is Cancel, then the receipt after it leaves
the alternate screen."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/release_board.py", work])
    d.wait_text(p, "Down/Enter:")
    d.send_and_wait_text(p, ["Down"], "Enter: inspect beta")
    d.send_and_wait_text(p, ["Enter"], "page 1/2")
    d.send_and_wait_text(p, ["PageDown"], "page 2/2")
    code = re.findall(r"Authorization: (\w+)", d.screen(p))[-1]
    d.send_and_wait_text(p, ["q"], "Edit environment>")
    d.send_and_wait_text(p, ["C-u", "text:production", "Tab"], "Space: toggle audit")
    d.send_and_wait_text(p, ["Space"], "Tab/Enter: Apply")
    d.send_and_wait_text(p, ["Tab", "Enter"], "Right: select Confirm")
    d.send_and_wait_text(p, ["Right"], "Enter: confirm")
    d.send_and_wait_text(p, ["Enter"], "Code from details page 2>")
    d.text(p, code)
    d.keys(p, "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the board exited {status}"
    receipt = re.findall(r"RELEASED (\S+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(receipt + "\n")
