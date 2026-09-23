#!/bin/sh
# Hunt 8, finding 015 (class 6: a silent invariant break).
#
# fux reads its `Settings` resource with `World::resource` from a dozen
# systems -- painting the bar, running commands, spawning panes. That method
# panics when the resource is absent. Removing `Settings` over BRP with
# `world.remove_resources` therefore left a server that still answered
# `rpc.discover` and `world.query`, but painted nothing: every `fux.frame`
# failed inside the fallback error handler, logged and swallowed, with no
# notice to the attached session. A server that answers is not a server that
# serves, and nothing said it had stopped.
#
# The README documents raw resource mutation as trusted low-level access. This
# is not about forbidding it: it is that the failure was silent. Removal now
# restores `Settings` to its default and logs it, so the frontend keeps
# working and there is a record.
#
# This script removes `Settings`, then asks for a frame and checks the painted
# chrome is still there.
#
# Usage: 015-removing-settings-stops-painting-silently.sh /path/to/fux
# Exit 0: reproduced (a frame after the removal no longer paints its chrome).
# Exit 1: verified not reproduced (it still paints).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 removes an unrelated resource instead
# (fux::model::Wake is not one; a made-up name is refused), so painting is
# unaffected (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-015.XXXXXX)" || exit 2
cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

cat > "$DIR/fux.json" <<'JSON'
{"shell":["/bin/sh","-c","printf 'PANE\n'; exec cat"]}
JSON

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
time.sleep(0.5)

def paints():
    reply = rpc("fux.frame", {"viewer": viewer})
    return "main" in json.dumps(reply.get("result", {}))

if not paints():
    print("setup: the frame never painted its chrome before the removal", file=sys.stderr)
    sys.exit(2)

resource = "fux::model::Wake" if CONTROL else "fux::assets::Settings"
print("removing %s" % resource)
reply = rpc("world.remove_resources", {"resource": resource})
print("   reply: %s" % json.dumps(reply)[:100])
time.sleep(0.5)

if paints():
    print("server still paints its chrome")
    sys.exit(20)
print("REPRODUCED: after the removal a frame no longer paints its chrome")
sys.exit(10)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
