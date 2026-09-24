#!/bin/sh
# Hunt 8, finding 021 (class 6: accepted-looking input is lost).
#
# Input for a pane waited for its PTY writer in a queue of sixteen pieces,
# whatever their size. Once the PTY's own buffer was full -- the program
# busy, stopped, or just slower than the typing -- every piece past the
# sixteenth was refused ("sending into a full channel") and lost. Typed keys
# are one piece each, so seventeen keys were enough; a key arriving while
# the writer lagged was gone for good, with only the notice "sending into a
# full channel" to show for it.
#
# This script stops a pane's `cat`, sends eighty 2000-byte pastes (160,000
# bytes, more than a PTY holds plus sixteen pieces on Linux or macOS, and far
# under the byte bound), resumes it, and counts what `cat` received.
#
# Usage: 021-input-past-sixteen-pieces-is-lost.sh /path/to/fux
# Exit 0: reproduced (bytes were lost).
# Exit 1: verified not reproduced (every byte arrived, in order).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 leaves `cat` running, so nothing backs up (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-021.XXXXXX)" || exit 2
cleanup() {
  [ -f "$DIR/pid" ] && kill -CONT "$(cat "$DIR/pid")" 2>/dev/null
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

printf '{"shell":["/bin/sh","-c","stty raw -echo; echo $$ > pid; exec cat > got"]}\n' > "$DIR/fux.json"

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

time.sleep(0.3)
if not CONTROL:
    os.kill(pid, signal.SIGSTOP)
sent = b""
for i in range(80):
    piece = (b"%02d" % i) * 1000
    sent += piece
    rpc("world.trigger_event", {"event": "fux::control::UserInput", "value": {
        "viewer": viewer, "input": {"kind": "paste", "text": piece.decode()}}})
if not CONTROL:
    os.kill(pid, signal.SIGCONT)
got = b""
for _ in range(200):
    time.sleep(0.1)
    got = open(os.path.join(DIR, "got"), "rb").read()
    if len(got) >= len(sent):
        break
print("sent %d bytes in 80 pieces%s; the program received %d" % (len(sent), "" if CONTROL else " while it was stopped", len(got)))
if got != sent:
    print("REPRODUCED: input was lost")
    sys.exit(10)
print("every byte arrived, in order")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
