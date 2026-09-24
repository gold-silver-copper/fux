#!/bin/sh
# BRP policy work, finding 028 (class 6: fux stops obeying, silently).
#
# In Bevy 0.20 observers and registered systems are entities, and BRP could
# find them (world.list_components on nearby IDs lists
# bevy_ecs::observer::distributed_storage::Observer) and despawn them. fux
# routes every Control and UserInput event through observers, so after
# despawning them it kept answering and painting -- every invariant held --
# but ignored every command: a split did nothing.
#
# Exit 0: reproduced (observers were despawned and a split no longer splits).
# Exit 1: verified not reproduced (the despawns were refused; the split works).
# Exit 2: setup failure.
# NEGATIVE_CONTROL=1 splits without despawning anything.
#
# Usage: 028-despawning-fux-observers-silences-it.sh /path/to/fux
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-028.XXXXXX)" || exit 2
cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

printf '{"shell":["/bin/sh","-c","printf PANE; exec sleep 600"]}\n' > "$DIR/fux.json"

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


def call(method, params=None):
    return rpc(method, params)

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

def control(command):
    return rpc("world.trigger_event", {"event": "fux::control::Control", "value": {"viewer": viewer, "command": command}})

def query(component):
    return rpc("world.query", {"data": {"components": [component]}}).get("result") or []

def component(entity, name):
    r = rpc("world.get_components", {"entity": entity, "components": [name]})
    return (r.get("result") or {}).get("components", {}).get(name)

def settle():
    time.sleep(0.4)
    rpc("fux.frame", {"viewer": viewer})
    time.sleep(0.2)

despawned = 0
if not CONTROL:
    for index in range(0, 600):
        bits = 0xFFFFFFFF - index
        listed = rpc("world.list_components", {"entity": bits}).get("result")
        if isinstance(listed, list) and any("Observer" in c for c in listed):
            if "error" not in rpc("world.despawn_entity", {"entity": bits}):
                despawned += 1
before = len(query("fux::model::PaneView"))
control({"kind": "split", "axis": "horizontal", "program": None})
settle()
after = len(query("fux::model::PaneView"))
print("observers despawned:", despawned, "| panes before a split:", before, "after:", after)
if despawned and after == before:
    print("REPRODUCED: despawning fux's observers silenced its commands")
    sys.exit(10)
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
