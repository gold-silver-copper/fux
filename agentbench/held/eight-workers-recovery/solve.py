"""Eight workers at once, each answered as its own prompt asks: a retry
first where one is required, then its commit."""
import re


def solve(d, work, fixtures):
    workers = [d.spawn(["python3", "-I", f"{fixtures}/worker.py", work, str(i)]) for i in range(8)]
    for i, p in enumerate(workers):
        d.wait_text(p, "command:", timeout=20)
        retry = re.findall(rf"RETRY REQUIRED index={i}; command: (retry [0-9a-f]+)", d.screen(p, history=True))
        if retry:
            d.send_and_wait_text(p, ["text:" + retry[-1], "Enter"], "AUTHORIZE", timeout=20)
        commit = re.findall(rf"AUTHORIZE index={i}; command: (commit [0-9a-f]+)", d.screen(p, history=True))[-1]
        d.text(p, commit)
        d.keys(p, "Enter")
    results = []
    for i, p in enumerate(workers):
        d.wait_text(p, f"DONE index={i} result=", timeout=20)
        status = d.wait_exit(p)
        assert status == 0, f"worker {i} exited {status}"
        result = re.findall(rf"^DONE index={i} result=(W-[0-9a-f]+)$", d.screen(p, history=True), re.M)[-1]
        results.append(f"{i} {result}\n")
    with open(f"{work}/answer.txt", "w") as f:
        f.write("".join(results))
