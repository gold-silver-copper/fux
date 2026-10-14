"""Three programs side by side in one tab, each one's result passed to the
next as it comes."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True))[-1]


def solve(d, work, fixtures):
    run = lambda name: ["python3", "-I", f"{fixtures}/{name}.py", work]
    build = d.spawn(run("builder"), size=(180, 48))
    tests = d.split(build, run("test_runner"))
    preview = d.split(tests, run("preview"))
    d.wait_text(build, "BUILD READY", timeout=40)
    token = last(d, build, r"BUILD READY token=(B-[0-9a-f]+)")
    d.wait_text(tests, "BUILD TOKEN>")
    d.send_and_wait_text(tests, ["text:" + token, "Enter"], "TESTS PASSED", timeout=20)
    signature = last(d, tests, r"TESTS PASSED signature=(T-[0-9a-f]+)")
    d.wait_text(preview, "RELEASE PAIR>")
    d.send_and_wait_text(preview, [f"text:{token} {signature}", "Enter"], "PREVIEW LIVE")
    d.send_and_wait_text(preview, ["text:snapshot", "Enter"], "SNAPSHOT ticket=")
    ticket = last(d, preview, r"SNAPSHOT ticket=(S-[0-9a-f]+)")
    d.send_and_wait_text(preview, [f"text:shutdown {ticket}", "Enter"], "PREVIEW CLOSED")
    for p in (build, tests, preview):
        status = d.wait_exit(p)
        assert status == 0, f"{p} exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"build={token}\ntests={signature}\nsnapshot={ticket}\n")
