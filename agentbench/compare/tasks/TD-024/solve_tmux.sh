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

pane, raw = start("handoff", "handoff.py", True)
wait(raw, r"DRAFT INPUT OPEN: enter draft.txt lines, then \.END on its own line\.")
# A bracketed paste with CRLF exercises normalization; the terminator is submitted afterward.
draft = (work / "draft.txt").read_text().replace("\n", "\r\n")
tm("send-keys", "-t", pane, "-l", "--", "\x1b[200~" + draft + "\x1b[201~")
send(pane, ".END")
token = wait(raw, r"\x07ARMED token=([A-F0-9]{12})\r").group(1)
tm("send-keys", "-t", pane, "F2")
challenge = wait(raw, r"SEAL challenge=([A-F0-9]{15})\. Enter seal <challenge> <token>: ").group(1)
send(pane, "seal " + challenge + " " + token)
receipt = wait(raw, r"You said: seal " + challenge + " " + token + r"\r?\nSEALED receipt=([A-F0-9]{22})").group(1)
checksum = re.search(r"You said: \.END\r?\nDRAFT stored checksum=([A-F0-9]{20})", snapshot(raw)).group(1)
write_answer(["checksum=" + checksum, "token=" + token, "challenge=" + challenge, "receipt=" + receipt])
PY
