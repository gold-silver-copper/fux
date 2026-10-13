"""One bash for every step: a producer's status through script and tee
kept with PIPESTATUS, then passed on."""
import re
import shlex


def solve(d, work, fixtures):
    cli = f"python3 -I {shlex.quote(fixtures + '/buildctl.py')} {shlex.quote(work)}"
    sh = d.spawn(["bash"])  # the task asks for bash itself
    d.wait_idle(sh)
    d.run_command(sh, f"cd {shlex.quote(work + '/project')}; export BUILD_FLAVOR=strict")
    # script given no terminal to forward: it reads none, so the line ends as the producer does.
    _, lint = d.run_command(sh, f"script -q -e -c {shlex.quote(cli + ' lint')} /dev/null </dev/null"
                                f" | tee {shlex.quote(work + '/lint.txt')}; lint=${{PIPESTATUS[0]}}; (exit $lint)")
    _, recorded = d.run_command(sh, f'{cli} record-lint "$lint"')
    assert recorded == "0", f"record-lint exited {recorded}"
    body, compiled = d.run_command(sh, f"{cli} compile", timeout=20)
    artifact = re.findall(r"^ARTIFACT=(\S+)", body, re.M)[-1]
    _, final = d.run_command(sh, f'{cli} finalize "$lint" {compiled}')
    assert final == "0", f"finalize exited {final}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"lint={lint}\ncompile={compiled}\ncwd=project\nflavor=strict\nartifact={artifact}\n")
