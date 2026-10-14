"""Two journals left running while a terminal client comes and goes; the
nightwatch found again by its name, checkpointed and renamed."""
import re


def solve(d, work, fixtures):
    journal = ["python3", "-I", f"{fixtures}/journal.py", work]
    d.spawn(journal + ["nightwatch"], name="nightwatch")
    canary = d.spawn(journal + ["canary"], name="canary")
    d.reattach()
    night = d.find("nightwatch")
    d.wait_text(night, "Checkpoint>", timeout=20)
    d.reattach()
    night = d.find("nightwatch")
    ticket = re.findall(r"CHECKPOINT_READY ticket=(\w+)", d.screen(night, history=True))[-1]
    d.send_and_wait_text(night, [f"text:checkpoint {ticket}", "Enter"], "Rename>")
    d.send_and_wait_text(night, ["text:rename archive-nightwatch", "Enter"], "RENAMED archive-nightwatch")
    receipt = re.findall(r"CHECKPOINT_RECEIPT (\S+)", d.screen(night, history=True))[-1]
    d.wait_history(canary, "CANARY_RUNNING", timeout=20)
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"archive-nightwatch={receipt}\ncanary=running\n")
