"""Ctrl-C once to a loader, which exits on it, and once to a relay, which
ignores it and drains first: each status as the program exited with it."""
import re


def solve(d, work, fixtures):
    loader = d.spawn(["python3", "-I", f"{fixtures}/service.py", work, "loader"])
    d.wait_text(loader, "CANCELLATION WINDOW OPEN", timeout=20)
    d.keys(loader, "C-c")
    loader_status = d.wait_exit(loader)
    relay = d.spawn(["python3", "-I", f"{fixtures}/service.py", work, "relay"])
    d.wait_text(relay, "RELAY READY", timeout=20)
    d.send_and_wait_text(relay, ["C-c"], "DRAIN token=")
    token = re.findall(r"DRAIN token=([0-9a-f]+)", d.screen(relay, history=True))[-1]
    d.text(relay, f"drain {token}")
    d.keys(relay, "Enter")
    relay_status = d.wait_exit(relay, timeout=30)
    drained, ignored = re.findall(r"RELAY STOPPED cleanly drained=(\d+) ignored=(\d+)",
                                  d.screen(relay, history=True))[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"loader={loader_status}\nrelay={relay_status}\nignored={ignored}\ndrained={drained}\n")
