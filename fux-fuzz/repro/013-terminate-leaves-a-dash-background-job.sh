#!/bin/sh
# Hunt 7, finding 013 (class 3: a child outlives its owner).
#
# `terminate` on a pane sends `SIGHUP` to the pane's process group, waits
# 100 ms for the shell to hang up its own jobs, then `SIGKILL`s that group. A
# background job started by an interactive shell lives in a *different*
# process group, so neither signal reaches it. It dies only if the shell
# forwards the hangup.
#
# `bash` and `zsh` forward it. `dash` does not, and `dash` is `/bin/sh` on
# Debian and Ubuntu. So a pane running the system `sh` there leaves
# `sleep 600 &` running after the pane it belongs to is gone:
#
#   shell 27 bg 29 shell pgid 27 bg pgid 29
#   after terminate: shell alive False background alive True
#
# The job is reparented to init and is still the user's to kill, so nothing is
# hidden; the point is that the README says a pane's process is terminated
# with the pane, and under `dash` a job it started is not.
#
# `fux-fuzz/README.md` has said since an earlier hunt that "Ubuntu's dash
# /bin/sh does not provide bash's background-job SIGHUP propagation", and the
# harness fixture execs bash to avoid it. fux's own integration test
# `interactive_background_jobs_hang_up_when_pane_terminates` configures
# `/bin/sh`, which is bash on macOS and dash on Debian, which is why that test
# passes here and fails there.
#
# This script names `/bin/dash` explicitly rather than `/bin/sh`, so it tests
# the shell rather than the platform's choice of shell. macOS ships
# `/bin/dash` too, so the finding reproduces on both: it is a shell
# difference, not a platform one.
#
# Usage: 013-terminate-leaves-a-dash-background-job.sh /path/to/fux
# Exit 0: reproduced (the background job outlived the pane).
# Exit 1: verified not reproduced (it went with the pane).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 runs the same pane under bash, which does forward the
# hangup, so the job must die with its pane (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

if [ "${NEGATIVE_CONTROL:-0}" = "1" ]; then
  SHELL_BIN=/bin/bash
else
  SHELL_BIN=/bin/dash
fi
[ -x "$SHELL_BIN" ] || { echo "$SHELL_BIN is not available here" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-013.XXXXXX)" || exit 2

cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  # The job under test is the thing that may survive; take it with us.
  [ -n "${BGPID:-}" ] && kill -9 "$BGPID" 2>/dev/null
  if [ -f "$DIR/bg.pid" ]; then
    kill -9 "$(cat "$DIR/bg.pid" 2>/dev/null)" 2>/dev/null
  fi
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

printf '{"shell":["%s","-i"]}\n' "$SHELL_BIN" > "$DIR/fux.json"

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --socket "$DIR/s/fux.sock" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$DIR/s/fux.sock" DIR="$DIR" SHELL_BIN="$SHELL_BIN" python3 - <<'PY'
import json, os, socket, sys, time
SOCK, DIR = os.environ["SOCK"], os.environ["DIR"]
SHELL_BIN = os.environ["SHELL_BIN"]

def rpc(method, params=None, timeout=6.0):
    body = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        body["params"] = params
    payload = json.dumps(body).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(SOCK)
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\n"
                  b"Content-Length: %d\r\nConnection: close\r\n\r\n" % len(payload) + payload)
        raw = b""
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            raw += chunk
    finally:
        s.close()
    head, _, b = raw.partition(b"\r\n\r\n")
    if b"transfer-encoding: chunked" in head.lower():
        out = b""
        while b:
            size, _, rest = b.partition(b"\r\n")
            try:
                n = int(size, 16)
            except ValueError:
                break
            if n == 0:
                break
            out += rest[:n]
            b = rest[n + 2:]
        b = out
    return json.loads(b.decode())

for _ in range(250):
    try:
        rpc("rpc.discover", timeout=1.0)
        break
    except Exception:
        time.sleep(0.1)
else:
    print("setup: server never answered", file=sys.stderr)
    sys.exit(2)

viewer = rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"]
time.sleep(0.8)

def status():
    rows = rpc("world.query", {"data": {"components": ["fux::model::ProcessState"]}})["result"]
    return rows[0]["components"]["fux::model::ProcessState"]["status"]

shell_pid = status().get("pid")
if not shell_pid:
    print("setup: the pane shell never reported a pid (%s)" % json.dumps(status()), file=sys.stderr)
    sys.exit(2)

pid_file = os.path.join(DIR, "bg.pid")
command = "sleep 600 & echo $! > %s" % pid_file

def job_started():
    return os.path.exists(pid_file) and open(pid_file).read().strip()

# An interactive shell may not be reading yet; shell startup differs by shell
# and by machine. Send again rather than assume one attempt landed.
for _ in range(5):
    rpc("world.trigger_event", {"event": "fux::control::UserInput", "value": {
        "viewer": viewer, "input": {"kind": "paste", "text": command}}})
    # An explicit Enter, not a newline inside the paste: bash 5.1 and later
    # turn on bracketed paste, where a pasted newline goes into the line
    # buffer instead of running it. fux's own tests press Enter for the same
    # reason. dash does not, which is why only the control needed this.
    rpc("world.trigger_event", {"event": "fux::control::UserInput", "value": {
        "viewer": viewer, "input": {"kind": "key", "key": "enter",
                                    "ctrl": False, "alt": False, "shift": False}}})
    for _ in range(30):
        if job_started():
            break
        time.sleep(0.1)
    if job_started():
        break
else:
    print("setup: the background job never recorded its pid", file=sys.stderr)
    sys.exit(2)
background = int(open(pid_file).read().strip())

def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # A reaped-but-unwaited child would answer to signal 0; the job's parent
    # is the pane shell, which fux reaps, so this is enough here.
    return True

print("pane shell %s (%s), background job %d, same group: %s"
      % (shell_pid, SHELL_BIN, background,
         os.getpgid(shell_pid) == os.getpgid(background)))

rpc("world.trigger_event", {"event": "fux::control::Control",
                            "value": {"viewer": viewer, "command": {"kind": "terminate"}}})

deadline = time.time() + 5
while time.time() < deadline and alive(shell_pid):
    time.sleep(0.1)
time.sleep(1.0)

shell_gone = not alive(shell_pid)
job_alive = alive(background)
print("   after terminate: pane shell gone %s, background job alive %s" % (shell_gone, job_alive))

if not shell_gone:
    print("setup: the pane shell itself did not go away", file=sys.stderr)
    sys.exit(2)

if job_alive:
    try:
        os.kill(background, 9)
    except Exception:
        pass
    print("REPRODUCED: the job outlived the pane that started it")
    sys.exit(10)
print("NOT reproduced: the job went with its pane")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
