"""A retry, then a commit only once the program's own quiet stretch is
over, with the ticket it printed before it."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/import_queue.py", work])
    d.wait_text(p, "Command>", timeout=20)
    assert "CACHE_MISS" in d.screen(p, history=True), "the queue did not report CACHE_MISS"
    d.send_and_wait_text(p, ["text:retry", "Enter"], "QUIET_READY", timeout=20)
    d.wait_idle(p)  # its Commit> prompt
    ticket = re.findall(r"DRAINED ticket=(\w+)", d.screen(p, history=True))[-1]
    d.send_and_wait_text(p, [f"text:commit {ticket}", "Enter"], "RECEIPT")
    status = d.wait_exit(p)
    assert status == 0, f"the queue exited {status}"
    receipt = re.findall(r"RECEIPT (\S+)", d.screen(p, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(receipt + "\n")
