"""Sixteen workers kept alive while a collector takes each one's part, in
order, then publishes and releases them all."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    shards = f"{fixtures}/shards.py"
    workers = [d.spawn(["python3", "-I", shards, work, "worker", str(n)]) for n in range(16)]
    parts = []
    for n, p in enumerate(workers):
        # Ready from two to about twelve seconds after the fleet registers.
        d.wait_text(p, "READY_PART", timeout=40)
        part, token = re.findall(r"READY_PART (\d+) (\w+)", d.screen(p, history=True))[-1]
        assert int(part) == n, f"worker {n} reported part {part}"
        parts.append(f"part {n:02d} {token}")
    # Wide enough that the summary line is one row.
    collector = d.spawn(["python3", "-I", shards, work, "collector"], size=(160, 30))
    d.wait_text(collector, "Parts>")
    keys = [k for line in parts for k in (f"text:{line}", "Enter")]
    d.send_and_wait_text(collector, keys, "Publish>", timeout=30)
    ticket = last(d, collector, r"PUBLISH_READY ticket=(\w+)")
    d.send_and_wait_text(collector, [f"text:publish {ticket}", "Enter"], "SUMMARY")
    for p in [collector, *workers]:
        status = d.wait_exit(p)
        assert status == 0, f"a program exited {status}"
    summary = last(d, collector, r"^SUMMARY .+$")
    with open(f"{work}/answer.txt", "w") as f:
        f.write(summary.rstrip() + "\n")
