"""Eight programs at once, each answered at its own prompt; a retry only
for those that ask for one, the rest known by their exit."""
import re

from drivers import Exited


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True))[-1]


def solve(d, work, fixtures):
    workers = [d.spawn(["python3", "-I", f"{fixtures}/worker.py", work, str(i)]) for i in range(1, 9)]
    rows = []
    for i, p in enumerate(workers, 1):
        d.wait_text(p, "begin>", timeout=30)
        code = last(d, p, rf"BEGIN READY id={i} code=(\S+)")
        d.send_and_wait_text(p, [f"text:begin {code}", "Enter"], "Working id=")
        try:
            d.wait_text(p, "retry>", timeout=20)
        except Exited:
            pass  # done in one attempt
        else:
            token = last(d, p, rf"RETRY REQUIRED id={i} token=(\S+)")
            d.send_and_wait_text(p, [f"text:retry {token}", "Enter"], "DONE id=", timeout=20)
        status = d.wait_exit(p, timeout=20)
        assert status == 0, f"worker {i} exited {status}"
        value, attempts = re.findall(rf"DONE id={i} value=(\d+) attempts=(\d+)", d.screen(p, history=True))[-1]
        rows.append(f"{i}\t{value}\t{attempts}\n")
    with open(f"{work}/answer.txt", "w") as f:
        f.write("".join(rows))
