"""Two jobs started together, each answered as its own prompt says once a
definitive outcome comes (not the diagnostic or tentative lines), then
their footers and statuses read after both exit."""
import re


def solve(d, work, fixtures):
    jobs = {job: d.spawn(["python3", "-I", f"{fixtures}/maintenance.py", work, job])
            for job in ("index", "cache")}
    for job, p in jobs.items():
        d.wait_text(p, "reply:", timeout=20)
        reply = re.findall(r"^(?:READY token|FATAL code)=\S+; reply: (.+)$",
                           d.screen(p, history=True), re.M)[-1].strip()
        d.send_and_wait_text(p, [f"text:{reply}", "Enter"], f"FINAL {job} receipt")
    rows = []
    for job, p in jobs.items():
        status = d.wait_exit(p)
        receipt = re.findall(rf"FINAL {job} receipt=(\S+)", d.screen(p, history=True))[-1]
        rows.append(f"{job} {receipt} {status}\n")
    with open(f"{work}/answer.txt", "w") as f:
        f.write("".join(rows))
