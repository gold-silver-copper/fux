"""Eighteen programs at once: all started before any is dispatched, every
result in before any commit, the reduction computed from the RESULT lines."""
import hashlib
import json
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    shards = [d.spawn(["python3", "-I", f"{fixtures}/shard_scan.py", work, str(i)]) for i in range(18)]
    for p in shards:
        d.wait_text(p, "scan>", timeout=20)
    for i, p in enumerate(shards):
        nonce = last(d, p, rf"^READY shard={i} nonce=([0-9a-f]+)")
        d.send_and_wait_text(p, [f"text:scan {nonce}", "Enter"], f"Shard {i} scan")
    results = []
    for i, p in enumerate(shards):
        d.wait_text(p, "commit>", timeout=20)
        count, digest = last(d, p, rf"^RESULT shard={i} count=(\d+) digest=([0-9a-f]+)")
        results.append({"id": i, "count": int(count), "digest": digest})
    text = ",".join(f"{s['id']}:{s['count']}:{s['digest']}" for s in results)
    root = hashlib.sha256(text.encode()).hexdigest()
    with open(f"{work}/answer.txt", "w") as f:
        f.write(json.dumps({"shards": results, "total": sum(s["count"] for s in results), "root": root}) + "\n")
    for i, p in enumerate(shards):
        d.send_and_wait_text(p, [f"text:commit {root}", "Enter"], f"COMMITTED shard={i}")
    for i, p in enumerate(shards):
        status = d.wait_exit(p)
        assert status == 0, f"shard {i} exited {status}"
