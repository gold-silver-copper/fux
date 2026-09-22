#!/bin/sh
# Hunt 6, finding 005 (class 1: the server aborts).
#
# `world.mutate_components` on an entity that does not exist panics inside
# bevy_remote instead of returning a JSON-RPC error:
#
#   thread 'main' panicked at bevy_remote-0.19.1/src/builtin_methods.rs:1194:28
#   Entity not yet spawned: The entity with ID ...v0 is not spawned
#   Encountered a panic in system `bevy_remote::process_remote_requests`!
#
# The method resolves the component, then calls `world.entity_mut(entity)`
# without checking the entity first, so any id that names no live entity ends
# the server. Both a never-allocated id and a despawned one reach it, and a
# despawned one is the ordinary case: a caller that reads an id, has it closed
# underneath, and writes back kills the session.
#
# The defect is upstream, but fux serves the stock registry unfiltered, so it
# is reachable through fux with one accepted request. Every sibling method
# (`world.get_components`, `world.insert_components`, `world.remove_components`)
# refuses the same id with a typed error, so this is the odd one out.
#
# Usage: 005-mutate-components-missing-entity-aborts.sh /path/to/fux
# Exit 0: reproduced (the server aborted).
# Exit 1: verified not reproduced (the server still answers).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 mutates a live entity instead, which must leave the
# server answering (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-005.XXXXXX)" || exit 2
SOCK="$DIR/s/fux.sock"
cat > "$DIR/default-shell" <<'SH'
#!/bin/sh
echo $$ > initial.pid
printf 'DEFAULT-SHELL\n'
exec /bin/bash --noprofile --norc -i
SH
chmod 700 "$DIR/default-shell"

cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL="$DIR/default-shell" TERM=xterm-256color \
  PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --socket "$SOCK" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$SOCK" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
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
    print("setup: server never answered on %s" % SOCK, file=sys.stderr)
    sys.exit(2)

try:
    live = rpc("world.spawn_entity",
               {"components": {"bevy_ecs::name::Name": "probe"}})["result"]["entity"]
except Exception as e:
    print("setup: %s" % e, file=sys.stderr)
    sys.exit(2)
if CONTROL:
    target = live
    print("negative control: mutating live entity %d" % target)
else:
    target = rpc("world.spawn_entity", {"components": {}})["result"]["entity"]
    rpc("world.despawn_entity", {"entity": target})
    time.sleep(0.3)
    print("mutating despawned entity %d" % target)
try:
    reply = rpc("world.mutate_components", {"entity": target,
                "component": "bevy_ecs::name::Name", "path": "", "value": "renamed"})
    print("   reply: %s" % json.dumps(reply)[:120])
except Exception as e:
    print("   request failed at the transport: %s" % e)
time.sleep(1.5)
try:
    rpc("rpc.discover", timeout=3.0)
except Exception as e:
    print("REPRODUCED: the server stopped answering after the mutate (%s)" % e)
    sys.exit(10)
print("server still answers")
sys.exit(20)
PY
RESULT=$?

if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
sleep 0.3
if grep -q "panicked at" "$DIR/err" 2>/dev/null; then
  echo "REPRODUCED: server panicked"
  echo "--- stderr tail ---"; grep -m3 "panicked at\|Encountered\|Resource entities" "$DIR/err"
  exit 0
fi
if ! kill -0 "$SPID" 2>/dev/null; then
  echo "REPRODUCED: server process exited"; tail -4 "$DIR/err"; exit 0
fi
case "$RESULT" in
  10) exit 0 ;;
  20) echo "NOT reproduced: the mutate was refused and the server still answers"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
