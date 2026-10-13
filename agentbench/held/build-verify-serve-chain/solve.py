"""Three programs, each started on the one before's output and kept alive
until the last is done; then closed in the reverse order."""
import re


def last(d, p, pattern):
    return re.findall(pattern, d.screen(p, history=True), re.M)[-1]


def solve(d, work, fixtures):
    build = d.spawn(["python3", "-I", f"{fixtures}/build.py", work])
    d.wait_text(build, "Build profile?")
    d.send_and_wait_text(build, ["text:build silver", "Enter"], "Build lease held", timeout=30)
    artifact = last(d, build, r"^ARTIFACT ([0-9a-f]+)$")
    verify = d.spawn(["python3", "-I", f"{fixtures}/verify.py", work, artifact])
    d.wait_text(verify, "Verifier lease held", timeout=30)
    proof = last(d, verify, r"^PROOF ([0-9a-f]+)$")
    server = d.spawn(["python3", "-I", f"{fixtures}/server.py", work, artifact, proof])
    d.wait_text(server, "request:", timeout=30)
    d.send_and_wait_text(server, ["text:GET /health", "Enter"], "request:")
    nonce = last(d, server, r"^200 HEALTH nonce=([0-9a-f]+)$")
    d.send_and_wait_text(server, [f"text:POST /publish {nonce} {proof}", "Enter"], "quit closes server")
    for p, word in [(server, "quit"), (verify, "release"), (build, "release")]:
        d.text(p, word)
        d.keys(p, "Enter")
        status = d.wait_exit(p)
        assert status == 0, f"a program exited {status}"
    with open(f"{work}/answer.txt", "w") as f:
        f.write(f"health={nonce}\nartifact={artifact}\nproof={proof}\n")
