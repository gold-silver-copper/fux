"""An assistant's prompts, each answered once it is asked, through a lock
and its recovery, to a result accepted by its digest."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/planner_agent.py", work])
    d.wait_text(p, "Plan approval>", timeout=20)
    d.send_and_wait_text(p, ["text:approve workspace", "Enter"], "Read permission>")
    d.send_and_wait_text(p, ["text:allow manifest.txt", "Enter"], "Delete permission>")
    d.send_and_wait_text(p, ["text:deny delete", "Enter"], "Blocked action>", timeout=20)
    d.send_and_wait_text(p, ["text:inspect lock", "Enter"], "Recovery choice>")
    d.send_and_wait_text(p, ["text:retry serial", "Enter"], "Accept result>", timeout=20)
    result, digest = re.findall(r"RESULT REVIEW (\S+) digest=([0-9a-f]+)", d.screen(p, history=True))[-1]
    d.send_and_wait_text(p, [f"text:accept {digest}", "Enter"], "ASSISTANT COMPLETE")
    status = d.wait_exit(p)
    assert status == 0, f"the assistant exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"plan=workspace\npermission=manifest-only\nrecovery=serial\nresult={result}\n")
