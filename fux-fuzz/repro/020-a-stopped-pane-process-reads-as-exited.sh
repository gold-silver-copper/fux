#!/bin/sh
# Hunt 8, finding 020 (class 6: a live process is reported, and treated, as exited).
#
# fux waits for a pane's leader with waitid(P_PID, pid, WEXITED | WNOWAIT).
# macOS's waitid also reports a child that has merely stopped, although only
# WEXITED was asked for, and fux read that report as an exit: 128 + SIGSTOP
# (145). The pane showed its process exited, and closing or respawning it
# killed a process that was only stopped. Ctrl-Z in a pane running a program
# directly (no job-control shell), or `kill -STOP`, was enough. Linux's waitid
# does not report stops here, so Linux gives exit 1 unfixed as well.
#
# This script starts a pane whose program is `cat`, stops it with SIGSTOP,
# and reads the pane's ProcessState a second later.
#
# Usage: 020-a-stopped-pane-process-reads-as-exited.sh /path/to/fux
# Exit 0: reproduced (the stopped process reads as exited).
# Exit 1: verified not reproduced (it reads as running).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 does not stop the process (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-020.XXXXXX)" || exit 2
cleanup() {
  [ -f "$DIR/pid" ] && kill -CONT "$(cat "$DIR/pid")" 2>/dev/null
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

printf '{"shell":["/bin/sh","-c","stty raw -echo; echo $$ > pid; exec cat"]}\n' > "$DIR/fux.json"

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color \
  "$FUX" server --socket "$DIR/s/fux.sock" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$DIR/s/fux.sock" DIR="$DIR" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, signal, socket, sys, time
SOCK = os.environ["SOCK"]
DIR = os.environ["DIR"]
CONTROL = os.environ["CONTROL"] == "1"

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

viewer = None
for _ in range(250):
    try:
        viewer = rpc("fux.attach", {"rows": 24, "cols": 80}, timeout=1.0)["result"]["viewer"]
        break
    except Exception:
        time.sleep(0.1)
pid = None
for _ in range(100):
    try:
        pid = int(open(os.path.join(DIR, "pid")).read())
        break
    except Exception:
        time.sleep(0.05)
if viewer is None or pid is None:
    print("setup: no pane process", file=sys.stderr)
    sys.exit(2)

def status():
    rows = rpc("world.query", {"data": {"components": ["fux::model::ProcessState"]}})["result"]
    return rows[0]["components"]["fux::model::ProcessState"]["status"]

if status().get("kind") != "running":
    print("setup: the pane process is not running", file=sys.stderr)
    sys.exit(2)
if not CONTROL:
    os.kill(pid, signal.SIGSTOP)
time.sleep(1.0)
rpc("fux.frame", {"viewer": viewer})
seen = status()
print("after %s: %s" % ("waiting" if CONTROL else "SIGSTOP", json.dumps(seen)))
if seen.get("kind") != "running":
    print("REPRODUCED: a stopped pane process reads as ended")
    sys.exit(10)
print("the stopped process still reads as running")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
