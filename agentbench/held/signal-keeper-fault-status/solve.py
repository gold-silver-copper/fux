"""Ctrl-C a keeper that ignores the first, drain it, Ctrl-C again; trip a
probe that dies of SIGTERM. Each runs under a shell that reports the
status it really exited with."""
import re
import shlex


def solve(d, work, fixtures):
    lab = f"python3 -I {shlex.quote(fixtures + '/signal_lab.py')} {shlex.quote(work)}"

    def under_shell(role):
        return d.spawn(["bash", "-c", f"{lab} {role}; rc=$?; {lab} report {role} \"$rc\""])

    keeper = under_shell("keeper")
    fault = under_shell("fault")
    d.wait_text(keeper, "Keeper>", timeout=20)
    d.send_and_wait_text(keeper, ["C-c"], "SIGINT ignored #1")
    d.send_and_wait_text(keeper, ["text:drain", "Enter"], "Draining journal")
    d.wait_text(fault, "Fault>", timeout=20)
    d.send_and_wait_text(fault, ["text:trip", "Enter"], "EXIT fault=")
    assert d.wait_exit(fault) == 0, "the fault's shell failed"
    fault_status = re.findall(r"EXIT fault=(\d+)", d.screen(fault, history=True))[-1]
    d.wait_text(keeper, "SHUTDOWN READY", timeout=20)
    d.send_and_wait_text(keeper, ["C-c"], "EXIT keeper=")
    assert d.wait_exit(keeper) == 0, "the keeper's shell failed"
    text = d.screen(keeper, history=True)
    keeper_status = re.findall(r"EXIT keeper=(\d+)", text)[-1]
    interrupts = re.findall(r"KEEPER CLOSED code=\d+ interrupts=(\d+)", text)[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"keeper={keeper_status}\nfault={fault_status}\ninterrupts={interrupts}\n")
