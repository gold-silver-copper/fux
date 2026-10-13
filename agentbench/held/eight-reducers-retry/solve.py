"""Eight programs at once, each answered with what it asks for itself: a
retry first for some, then a collect; results gathered in index order."""
import re

RETRY, COMMIT = "RETRY_REQUIRED R", "COMMIT_READY C"


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def first_request(d, p):
    """Which the reducer asks for first; `COMMIT_READY?` is not a request."""
    if hasattr(d, "wait_any_text"):
        return d.wait_any_text(p, [RETRY, COMMIT], timeout=30)
    return d.poll(lambda: next((t for t in (RETRY, COMMIT) if t in d.screen(p)), None), 30)


def solve(d, work, fixtures):
    reducers = [d.spawn(["python3", "-I", f"{fixtures}/reducer.py", work, str(i)]) for i in range(1, 9)]
    rows = []
    for i, p in enumerate(reducers, 1):
        if first_request(d, p) == RETRY:
            token = last(d, p, r"^RETRY_REQUIRED (\S+)")
            d.send_and_wait_text(p, [f"text:retry {token}", "Enter"], COMMIT, timeout=30)
        token = last(d, p, r"^COMMIT_READY (C\S+)")
        d.send_and_wait_text(p, [f"text:collect {token}", "Enter"], "RESULT")
        status = d.wait_exit(p)
        assert status == 0, f"reducer {i} exited {status}"
        rows.append(last(d, p, rf"^RESULT ({i}:\d+)$"))
    with open(f"{work}/answer.txt", "w") as f:
        f.write("".join(r + "\n" for r in rows))
