#!/bin/sh
# Hunt 7, finding 009 (class 1: the server aborts).
#
# Hunt 6 finding 004 was `world.despawn_entity` on a resource entity. fux now
# guards that method: it refuses an entity holding `IsResource`. But fux
# despawns viewers itself, and those paths never ask. A caller does not need
# the guarded method at all:
#
#   1. `world.insert_components` puts a `Viewer` on a resource entity. Nothing
#      refuses it: the normalization that strips a `Viewer` from a layout node
#      (hunt 5 finding 001) knows about `Workspace`, `Tab`, `Split` and
#      `PaneView`, and a resource entity is none of them.
#   2. Opening a `fux.frame+watch` on that id and closing the connection makes
#      `disconnected` despawn it, because it now really is a viewer:
#      `IsViewer` is `(With<Viewer>, NotLayout)`.
#
# The despawn lands on a resource entity, which is what the guard exists to
# prevent:
#
#   WARN bevy_ecs::resource: Resource entities are not supposed to be despawned.
#   ERROR bevy_ecs::error::handler: Encountered an error in system
#     `bevy_app::main_schedule::Main::run_main`: System panicked
#   resource does not exist: bevy_app::main_schedule::MainScheduleOrder
#
# `Entity::to_bits` complements the index, so the resource entities are a short
# fixed sequence from 0xFFFFFFFF downwards; a caller needs no guesswork. Their
# contents decide what happens:
#
#   index 1  Schedules              the server stops answering
#   index 3  MainScheduleOrder      the server stops answering
#   index 2  AppTypeRegistry        the server answers every request with an
#                                   error: alive, useless, and still holding
#                                   every session's PTYs
#
# fux's own `detach` command reaches the same despawn when the id also carries
# a `Viewing` relationship, so this is the class, not the instance: every
# internal despawn of a caller-named entity needs the check the BRP method
# already makes.
#
# Usage: 009-viewer-on-a-resource-entity-ends-the-server.sh /path/to/fux
# Exit 0: reproduced (the server stopped answering).
# Exit 1: verified not reproduced (the server still answers).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 puts the `Viewer` on an ordinary spawned entity instead
# and watches that, which must leave the server answering (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-009.XXXXXX)" || exit 2
SOCK="$DIR/s/fux.sock"
cat > "$DIR/fux.json" <<'JSON'
{"shell":["/bin/sh","-c","printf 'PANE\n'; exec cat"]}
JSON

cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color \
  "$FUX" server --socket "$SOCK" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$SOCK" ERR="$DIR/err" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, time
SOCK = os.environ["SOCK"]
CONTROL = os.environ["CONTROL"] == "1"

def send(body, timeout=6.0, hold=None):
    payload = json.dumps(body).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(SOCK)
        close = b"" if hold is not None else b"Connection: close\r\n"
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\n"
                  b"Content-Length: %d\r\n" % len(payload) + close + b"\r\n" + payload)
        if hold is not None:
            # A streaming request: let it start, then close, which is the
            # disconnection the server reacts to.
            time.sleep(hold)
            return None
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

def rpc(method, params=None, timeout=6.0):
    body = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        body["params"] = params
    return send(body, timeout)

for _ in range(250):
    try:
        rpc("rpc.discover", timeout=1.0)
        break
    except Exception:
        time.sleep(0.1)
else:
    print("setup: server never answered on %s" % SOCK, file=sys.stderr)
    sys.exit(2)

if CONTROL:
    target = rpc("world.spawn_entity", {"components": {}})["result"]["entity"]
    print("negative control: an ordinary spawned entity %d" % target)
else:
    # Entity index 3 on this build holds MainScheduleOrder, without which the
    # main schedule cannot run. Index 1 (Schedules) does as well.
    target = 0xFFFFFFFF - 3
    print("resource entity, bits 0x%X (entity index 3)" % target)
    listed = rpc("world.list_components", {"entity": target}).get("result")
    print("   it holds: %s" % json.dumps(listed)[:160])

reply = rpc("world.insert_components", {"entity": target, "components": {
    "fux::model::Viewer": {"rows": 24, "cols": 80, "zoom": False, "scrollback": 0, "notice": None}}})
print("   insert a Viewer on it: %s" % json.dumps(reply)[:120])
time.sleep(0.5)

print("   open a frame watch naming it, then close the connection")
send({"jsonrpc": "2.0", "id": 2, "method": "fux.frame+watch", "params": {"viewer": target}}, hold=1.2)
time.sleep(1.5)

try:
    rpc("rpc.discover", timeout=3.0)
except Exception as e:
    print("REPRODUCED: the server stopped answering (%s)" % e)
    sys.exit(10)

# Alive is not the same as serving: despawning AppTypeRegistry leaves a server
# that answers every request with an error.
failed = []
for method, params in (("fux.frame", {"viewer": 0}), ("world.query", {"data": {"components": ["fux::model::Workspace"]}}),
                       ("registry.schema", None)):
    try:
        reply = rpc(method, params, timeout=3.0)
        if "error" in reply and method != "fux.frame":
            failed.append("%s: %s" % (method, json.dumps(reply["error"])[:60]))
    except Exception as e:
        failed.append("%s: %s" % (method, e))
if failed:
    print("REPRODUCED: the server answers but cannot serve: %s" % "; ".join(failed))
    sys.exit(10)
print("server still answers and still serves")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
