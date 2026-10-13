"""Ctrl-C to a shell's foreground job, its status kept by the shell; Ctrl-C
to a program that ignores it, then SIGTERM to the pid it printed."""
import re


def solve(d, work, fixtures):
    journal = d.spawn(["python3", "-I", f"{fixtures}/journal.py", work])
    sh = d.shell()
    d.wait_idle(sh)
    d.send_and_wait_text(sh, [f"text:python3 -I {fixtures}/foreground.py {work} run; job=$?", "Enter"],
                         "JOB ARMED", timeout=20)
    d.send_and_wait_text(sh, ["C-c"], "JOB INTERRUPTED")
    job, _ = d.run_command(sh, 'echo "$job"')
    _, recovery = d.run_command(sh, f"python3 -I {fixtures}/foreground.py {work} recover")
    d.wait_text(journal, "JOURNAL ARMED", timeout=20)
    pid = re.findall(r"JOURNAL ARMED pid=(\d+)", d.screen(journal, history=True))[-1]
    d.send_and_wait_text(journal, ["C-c"], "IGNORED SIGINT count=1")
    d.run_command(sh, f"kill -TERM {pid}")
    status = d.wait_exit(journal, timeout=20)
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"job={job.strip()}\nrecovery={recovery}\njournal={status}\n")
