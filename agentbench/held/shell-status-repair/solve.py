"""One shell: a failing command's real status carried into the next
command, which prompts while it runs."""
import re
import shlex


def solve(d, work, fixtures):
    cli = f"python3 -I {shlex.quote(fixtures + '/shell_job.py')} {shlex.quote(work)}"
    p = d.shell()
    d.wait_idle(p)
    d.run_command(p, f"export HISTFILE={shlex.quote(work + '/.bash_history')} BATCH_TAG='tidal batch'; "
                     f"mkdir -p {shlex.quote(work + '/project')} && cd {shlex.quote(work + '/project')}")
    _, ingest = d.run_command(p, f"{cli} ingest", timeout=30)
    assert ingest not in (None, "0"), f"ingest exited {ingest}"
    # The repair prompts while it runs: typed, answered, and its status
    # printed by the shell once it ends.
    d.send_and_wait_text(p, [f"text:{cli} repair {ingest}; echo REPAIR_STATUS=$?", "Enter"],
                         "Repair action >", timeout=30)
    d.send_and_wait_text(p, [f"text:retry {ingest}", "Enter"], "REPAIR_STATUS=", timeout=30)
    repair = re.findall(r"^REPAIR_STATUS=(\d+)$", d.screen(p, history=True), re.M)[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"ingest={ingest}\nrepair={repair}\ntag=tidal batch\ncwd=project\n")
