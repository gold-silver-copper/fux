"""A watcher left running under a name while another program runs, then
found again by that name: the same process, never restarted."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/archive.py", work, "watch"], name="archive-watch")
    d.wait_text(p, "WATCH READY ticket=", timeout=20)
    ticket = last(d, p, r"^WATCH READY ticket=([0-9a-f]+)$")
    # PARKED is written over the echo of what was typed (a bare CR): a wait
    # of its own, not one armed with the keys, which skips the echo's line.
    d.text(p, f"park {ticket}")
    d.keys(p, "Enter")
    d.wait_text(p, "PARKED")
    m = d.spawn(["python3", "-I", f"{fixtures}/archive.py", work, "maintain"], name="maintenance")
    d.wait_text(m, "MAINTENANCE COMPLETE", timeout=30)
    status = d.wait_exit(m)
    assert status == 0, f"maintenance exited {status}"
    receipt = last(d, m, r"^MAINTENANCE COMPLETE receipt=([0-9a-f]+)$")
    p = d.find("archive-watch")
    d.wait_text(p, "RESUME AVAILABLE code=", timeout=20)
    code = last(d, p, r"^RESUME AVAILABLE code=([0-9a-f]+)$")
    d.send_and_wait_text(p, [f"text:resume {code}", "Enter"], "STOP PROMPT")
    d.text(p, f"stop {ticket}")
    d.keys(p, "Enter")
    status = d.wait_exit(p)
    assert status == 0, f"the watcher exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"ticket={ticket}\nmaintenance={receipt}\nresume={code}\n")
