"""A service, a build it waits for and a probe of it, each started on the
one before's output; the service stopped with the probe's receipt."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    def start(name, *args):
        return d.spawn(["python3", "-I", f"{fixtures}/{name}.py", work, *args])

    service = start("service")
    d.wait_text(service, "WAITING_ARTIFACT")
    builder = start("builder")
    d.wait_text(builder, "BUILD_CHALLENGE=", timeout=20)
    challenge = last(d, builder, r"^BUILD_CHALLENGE=([^;]+);")
    d.send_and_wait_text(builder, [f"text:build {challenge}", "Enter"], "ARTIFACT_COMPLETE=", timeout=20)
    status = d.wait_exit(builder)
    assert status == 0, f"the builder exited {status}"
    artifact = last(d, builder, r"^ARTIFACT_COMPLETE=(\S+)")
    d.wait_text(service, "LOAD_PROMPT")
    d.send_and_wait_text(service, [f"text:load {artifact}", "Enter"], "CONTROL_PROMPT", timeout=20)
    ready, endpoint = re.findall(r"^SERVICE_READY token=(\S+) endpoint=(\S+)",
                                 d.screen(service, history=True), re.M)[-1]
    probe = start("probe", ready)
    d.wait_text(probe, "CHECK_PROMPT", timeout=20)
    d.send_and_wait_text(probe, [f"text:check {endpoint}", "Enter"], "PROBE_RECEIPT=", timeout=20)
    status = d.wait_exit(probe)
    assert status == 0, f"the probe exited {status}"
    receipt = last(d, probe, r"^PROBE_RECEIPT=(\S+)")
    d.text(service, f"stop {receipt}")
    d.keys(service, "Enter")
    status = d.wait_exit(service, timeout=20)
    assert status == 0, f"the service exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"ARTIFACT={artifact}\nPROBE={receipt}\nSERVICE=stopped\n")
