"""The first line that starts with a real outcome, past quoted decoys and
redraws; recovery answered only once it says it is idle."""
import re


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/scanner.py", work])
    # The outcome comes after about eight quiet seconds of progress redraws.
    d.wait_text(p, "decision>", timeout=30)
    outcome, ticket = re.findall(r"^OUTCOME (READY|FAILED) ticket=(\S+)",
                                 d.screen(p, history=True), re.M)[0]
    if outcome == "READY":
        d.send_and_wait_text(p, [f"text:accept {ticket}", "Enter"], "SCAN COMPLETE", timeout=30)
    else:
        d.send_and_wait_text(p, [f"text:recover {ticket}", "Enter"], "continuation>", timeout=30)
        generation = re.findall(r"^RECOVERY IDLE generation=(\d+)", d.screen(p, history=True), re.M)[-1]
        d.send_and_wait_text(p, [f"text:continue {generation}", "Enter"], "SCAN COMPLETE", timeout=30)
    status = d.wait_exit(p)
    assert status == 0, f"the scanner exited {status}"
    digest = re.findall(r"^SCAN COMPLETE digest=(\S+)", d.screen(p, history=True), re.M)[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"{outcome}|{ticket}|{digest}\n")
