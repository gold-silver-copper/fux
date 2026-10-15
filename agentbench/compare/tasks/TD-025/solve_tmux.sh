#!/usr/bin/env bash
set -euo pipefail
WORK=$1
FIXTURES=$2
trap 'tmux -L ab kill-server >/dev/null 2>&1 || true' EXIT
python3 - "$WORK" "$FIXTURES" <<'PY'
import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path

work, fixtures = map(lambda p: Path(p).resolve(), sys.argv[1:3])
all_outputs = []


def tm(*args):
    return subprocess.check_output(["tmux", "-L", "ab", *args], text=True).strip()


def start(name, script, first=False):
    raw = work / (name + ".terminal.log")
    raw.write_bytes(b"")
    if first:
        pane = tm("new-session", "-d", "-s", "reference", "-x", "100", "-y", "30",
                  "-P", "-F", "#{pane_id}", "bash --noprofile --norc")
        tm("set-option", "-w", "-t", "reference:0", "remain-on-exit", "on")
        tm("set-option", "-t", "reference", "history-limit", "20000")
    else:
        pane = tm("split-window", "-d", "-t", "reference:0", "-P", "-F", "#{pane_id}",
                  "bash --noprofile --norc")
        tm("select-layout", "-t", "reference:0", "tiled")
    tm("pipe-pane", "-t", pane, "cat > " + shlex.quote(str(raw)))
    command = shlex.join(["python3", "-I", str(fixtures / script), str(work)])
    command += "; printf '\\nREFERENCE PROCESS EXIT=%s\\n' \"$?\""
    tm("send-keys", "-t", pane, "-l", "--", command)
    tm("send-keys", "-t", pane, "Enter")
    all_outputs.append(raw)
    return pane, raw


def snapshot(raw):
    return raw.read_bytes().decode("utf-8", "replace")


def wait(raw, pattern):
    # Reference-only watchdog, not a fixture/task deadline.
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        output = snapshot(raw)
        if "INPUT REJECTED:" in output:
            raise RuntimeError(output)
        match = re.search(pattern, output)
        if match:
            return match
        time.sleep(0.025)
    raise RuntimeError("reference watchdog waiting for " + pattern + "\n" + snapshot(raw))


def send(pane, text):
    tm("send-keys", "-t", pane, "-l", "--", text)
    tm("send-keys", "-t", pane, "Enter")


def write_answer(lines):
    for raw in all_outputs:
        wait(raw, r"\nREFERENCE PROCESS EXIT=0\r?\n")
    (work / "answer.txt").write_text("\n".join(lines) + "\n", encoding="ascii")

bp, blog = start("builder", "builder.py", True)
sp, slog = start("server", "server.py")
pp, plog = start("probe", "probe.py")
wait(blog, r"LIVE BUILD: enter compile <target>, target is in manifest.txt: ")
send(bp, "compile " + (work / "manifest.txt").read_text().strip())
build = wait(blog, r"BUILT artifact=([A-F0-9]{14}) key=([A-F0-9]{12})\r")
artifact, key = build.groups()
nonce = wait(slog, r"LIVE SERVER READY nonce=([A-F0-9]{11})\. Enter attach <artifact> <key> after BUILT: ").group(1)
send(sp, "attach " + artifact + " " + key)
bound = wait(slog, r"attach " + artifact + " " + key + r" -> bound ticket=([A-F0-9]{15})").group(1)
ticket = wait(slog, r"SERVING ticket=([A-F0-9]{15})").group(1)
assert ticket == bound
wait(plog, r"LIVE PROBE: after SERVING, enter test <ticket> <nonce>: ")
send(pp, "test " + ticket + " " + nonce)
digest = wait(plog, r"\x07VERIFIED digest=([A-F0-9]{24})").group(1)
send(bp, "release " + digest)
send(sp, "release " + digest)
br = wait(blog, r"BUILDER CLOSED receipt=([A-F0-9]{13})").group(1)
sr = wait(slog, r"SERVER CLOSED receipt=([A-F0-9]{13})").group(1)
send(pp, "commit " + br + " " + sr)
final = wait(plog, r"COMMITTED receipt=([A-F0-9]{20})").group(1)
write_answer(["artifact=" + artifact, "ticket=" + ticket, "digest=" + digest,
              "builder=" + br, "server=" + sr, "final=" + final])
PY
