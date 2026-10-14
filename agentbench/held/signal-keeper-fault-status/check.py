import json, os, sys
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    fx = w / ".fx"
    def need(condition, reason):
        if not condition:
            raise ValueError(reason)
    def read(name):
        return json.loads((fx / (name + ".json")).read_text())
    def answer(expected):
        need((w / "answer.txt").read_bytes() == expected.encode("utf-8"), "answer.txt differs from the exact expected bytes")
    def stopped(name):
        pid = int((fx / (name + ".pid")).read_text())
        identity = read(name + ".process")
        need(identity.get("stdin_tty") is True and identity.get("stdout_tty") is True, name + " did not run in a terminal")
        need(read(name + ".launches") == 1, name + " must run exactly once")
        need(identity["pid"] == pid and isinstance(identity["start_ticks"], int), "process identity record differs")
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            return
        if int(fields[19]) != identity["start_ticks"] or fields[0] in ("Z", "X"):
            return
        raise ValueError(name + " is still running")
    try:
        need(fx.is_dir(), "no fixture records")
        need(not list(fx.glob("*.failure.json")), "fixture recorded a protocol failure")
        need(read("keeper") == {"interrupts": 2, "drained": True, "exit": 0}, "keeper interrupt/drain audit differs")
        need(read("fault") == {"trip": True, "signal": "SIGTERM"}, "fault was not triggered")
        need(read("signal-report-keeper") == {"status": 0} and read("signal-report-fault") == {"status": 143}, "signal exit statuses not reported")
        answer("keeper=0\nfault=143\ninterrupts=2\n")
        stopped("signal-keeper")
        stopped("signal-fault")
        stopped("signal-report-keeper")
        stopped("signal-report-fault")
        return True, "all required terminal interactions and final values match"
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == "__main__":
    ok, reason = check(sys.argv[1])
    print(("PASS: " if ok else "FAIL: ") + reason)
    raise SystemExit(0 if ok else 1)
