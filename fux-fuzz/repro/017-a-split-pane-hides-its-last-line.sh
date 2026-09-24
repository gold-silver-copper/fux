#!/bin/sh
# Hunt 8, finding 017 (class 6: a frame shows the wrong rows).
#
# fux_vt::Grid::resized kept the first `history + rows` rows, so a shrink cut
# rows off the bottom of the live area -- the cursor line -- instead of
# scrolling the top into history. A new pane's PTY starts at one size and the
# first frame resizes it to its rectangle, so a program whose output overflows
# the pane and whose last line has no newline lost that line: the pane showed
# the row above it. It is not a settling race: the dropped row is gone from the
# emulator, and no later frame brings it back. Earlier runs judged it
# nondeterministic only because they polled.
#
# This script splits the focused pane with a program that prints 40 lines and
# then ENDMARK without a newline, waits a fixed time, and takes ONE fux.frame.
#
# Usage: 017-a-split-pane-hides-its-last-line.sh /path/to/fux
# Exit 0: reproduced (ENDMARK is missing from the frame).
# Exit 1: verified not reproduced (the frame shows ENDMARK).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 ends the program's output with a newline, so the line a
# shrink drops is the empty cursor line and ENDMARK stays visible (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-017.XXXXXX)" || exit 2
cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

printf '{"shell":["/bin/sh","-c","printf PANE; exec cat"]}\n' > "$DIR/fux.json"

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color \
  "$FUX" server --socket "$DIR/s/fux.sock" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$DIR/s/fux.sock" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, time
SOCK = os.environ["SOCK"]
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
if viewer is None:
    print("setup: the server never accepted an attachment", file=sys.stderr)
    sys.exit(2)

end = "\\n" if CONTROL else ""
program = ("i=1; while [ $i -le 40 ]; do echo LINE-$i; i=$((i+1)); done; "
           "printf 'ENDMARK%s'; exec sleep 100" % end)
rpc("world.trigger_event", {"event": "fux::control::Control", "value": {
    "viewer": viewer, "command": {"kind": "split", "axis": "horizontal", "program": program}}})
# One frame after a fixed wait. Polling is what hid this: every request wakes
# the runner, so a loop cannot tell a stale frame from a lost row.
time.sleep(2.0)
paint = rpc("fux.frame", {"viewer": viewer})["result"]["paint"]
if "LINE-40" not in paint:
    print("setup: the split pane never painted its output", file=sys.stderr)
    sys.exit(2)
if "ENDMARK" in paint:
    print("the frame shows ENDMARK")
    sys.exit(20)
print("REPRODUCED: the split pane's frame lacks its last line, ENDMARK")
sys.exit(10)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
