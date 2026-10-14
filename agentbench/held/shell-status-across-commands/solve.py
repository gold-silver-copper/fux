"""One shell for every step; each command's status read from the shell, not
from text on the screen that looks like a prompt."""
import re
import shlex


def solve(d, work, fixtures):
    cli = f"python3 -I {shlex.quote(fixtures + '/batch_cli.py')} {shlex.quote(work)}"
    p = d.shell()
    d.wait_idle(p)
    _, status = d.run_command(p, f"{cli} begin")
    assert status == "0", f"begin exited {status}"
    d.run_command(p, f"export BATCH_LABEL='morning shift'; cd {shlex.quote(work + '/queue stage')}")
    body, status = d.run_command(p, f"{cli} calculate")
    total = re.findall(r"^CALCULATED SUM (-?\d+)$", body, re.M)[-1]
    body, final = d.run_command(p, f"{cli} finalize {status}")
    assert final == "0", f"finalize exited {final}"
    receipt = re.findall(r"^FINAL RECEIPT (\S+)$", body, re.M)[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"sum={total}\nexit={status}\nreceipt={receipt}\n")
