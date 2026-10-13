"""An assistant supervised prompt by prompt: a narrow approval, a revised
plan, live tokens read off the screen, and a stall answered from a file."""
import json
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/assistant.py", work])
    d.wait_text(p, "reply: allow scan", timeout=20)
    d.send_and_wait_text(p, ["text:allow scan", "Enter"], "Revise or approve plan:", timeout=20)
    d.send_and_wait_text(p, ["text:revise: preserve archive; regenerate index", "Enter"],
                         "Permission reply:", timeout=20)
    plan = last(d, p, r"^PLAN TOKEN=([0-9a-f]+)$")
    d.send_and_wait_text(p, [f"text:approve {plan} write:index.json", "Enter"],
                         "reply: provide metadata", timeout=30)
    with open(f"{work}/metadata.json") as f:
        meta = json.load(f)
    d.send_and_wait_text(p, [f"text:provide metadata {meta['project']} {meta['revision']}", "Enter"],
                         "Apply this diff?", timeout=20)
    token = last(d, p, r"^APPLY TOKEN=([0-9a-f]+)$")
    d.send_and_wait_text(p, [f"text:apply {token}", "Enter"], "Assistant idle.", timeout=20)
    digest = last(d, p, r"^COMPLETE index=([0-9a-f]+)$")
    d.text(p, "exit")
    d.keys(p, "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the assistant exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"index={digest}\n")
