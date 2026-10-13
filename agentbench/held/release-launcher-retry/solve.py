"""One shell at 96x28 for both launches: the failing one's status read from
the shell, the second answered at its prompt and its status echoed after."""
import re
import shlex


def solve(d, work, fixtures):
    launcher = f"python3 -I {shlex.quote(fixtures + '/launcher.py')} {shlex.quote(work)}"
    p = d.shell(size=(96, 28))
    d.wait_idle(p)
    d.run_command(p, f"export RELEASE_CHANNEL=amber; cd {shlex.quote(work + '/project')}")
    _, broken = d.run_command(p, f"{launcher} broken", timeout=20)
    # It prompts, so its status comes after: echoed, split so its echo is not the line.
    d.send_and_wait_text(p, [f"text:{launcher} release; echo RELEASE_''EXIT=$?", "Enter"],
                         "Publication response", timeout=20)
    tag = re.findall(r"STARTUP READY tag=(\S+)", d.screen(p, history=True))[-1]
    d.send_and_wait_text(p, [f"text:publish {tag}", "Enter"], "RELEASE_EXIT=")
    release = re.findall(r"^RELEASE_EXIT=(\d+)", d.screen(p, history=True), re.M)[-1]
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"{broken}\n{release}\n{tag}\n")
