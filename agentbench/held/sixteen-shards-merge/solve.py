"""Seventeen programs at once: each shard answered as it gets ready, then
accepted and merged in index order, whatever order they finish in."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True))[-1]


def solve(d, work, fixtures):
    shards = [d.spawn(["python3", "-I", f"{fixtures}/shard.py", work, str(i)]) for i in range(16)]
    merge = d.spawn(["python3", "-I", f"{fixtures}/merge.py", work])
    for p in shards:
        d.wait_text(p, "run>", timeout=20)
        challenge = last(d, p, r"READY shard-\d\d challenge=([0-9a-f]+)")
        d.send_and_wait_text(p, [f"text:run {challenge}", "Enter"], "DONE dry-run")
    for i, p in enumerate(shards):
        name = f"shard-{i:02d}"
        d.wait_text(p, "accept>", timeout=20)
        result = last(d, p, rf"RESULT {name}=([0-9a-f]+)")
        d.send_and_wait_text(p, [f"text:accept {result}", "Enter"], f"SHARD ACCEPTED {name}")
        status = d.wait_exit(p)
        assert status == 0, f"{name} exited {status}"
        d.wait_text(merge, f"collect {name}>")
        d.send_and_wait_text(merge, [f"text:{name} {result}", "Enter"], f"COLLECTED {name}")
    d.wait_text(merge, "confirm>")
    verify = last(d, merge, r"VERIFY key=([0-9a-f]+)")
    d.send_and_wait_text(merge, [f"text:confirm {verify}", "Enter"], "MERGE COMPLETE")
    status = d.wait_exit(merge)
    assert status == 0, f"the merge exited {status}"
    count, receipt = re.findall(r"MERGE COMPLETE count=(\d+) receipt=([0-9a-f]+)",
                                d.screen(merge, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"count={count}\nmerge={receipt}\n")
