"""An agent-like program supervised prompt by prompt: Escape once it is
blocked, then a second program beside it whose evidence unblocks it."""
import re


def solve(d, work, fixtures):
    a = d.spawn(["python3", "-I", f"{fixtures}/review_agent.py", work])
    d.wait_text(a, "PLAN>")
    d.send_and_wait_text(a, ["text:revise preserve-input", "Enter"], "WRITE_PERMISSION>")
    d.send_and_wait_text(a, ["text:approve report-only", "Enter"], "NETWORK_PERMISSION>")
    d.send_and_wait_text(a, ["text:deny", "Enter"], "BLOCKED offline cache lock", timeout=20)
    d.send_and_wait_text(a, ["Escape"], "OPERATOR>")
    d.send_and_wait_text(a, ["text:status", "Enter"], "RETRY>")
    v = d.split(a, ["python3", "-I", f"{fixtures}/offline_verifier.py", work])
    status = d.wait_exit(v, timeout=20)
    assert status == 0, f"the verifier exited {status}"
    evidence = re.findall(r"OFFLINE_EVIDENCE=(\S+)", d.screen(v, history=True))[-1]
    d.send_and_wait_text(a, [f"text:retry offline {evidence}", "Enter"], "REVIEW_DONE")
    status = d.wait_exit(a)
    assert status == 0, f"the agent exited {status}"
    reviewed, source = re.findall(r"REVIEW_DONE reviewed=(\d+) source=(\S+)", d.screen(a, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"reviewed={reviewed}\nsource={source}\n")
