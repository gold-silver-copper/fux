#!/bin/sh
# Hunt 6, finding 004 (class 1: the server aborts).
#
# Bevy 0.19 stores resources as entities, and fux exposes the stock BRP method
# registry unfiltered, so `world.despawn_entity` can despawn one. Despawning a
# resource entity leaves the ECS inconsistent; the next command flush panics
# inside `bevy_ecs`, and the panic takes the whole server down with every
# attached session and child process.
#
#   WARN bevy_ecs::resource: Resource entities are not supposed to be despawned.
#   thread 'main' panicked at bevy_ecs-0.19.1/src/error/handler.rs:130:1
#   Encountered a panic in system `bevy_remote::process_remote_requests`!
#
# `Entity::to_bits` is an opaque encoding whose low 32 bits are the bitwise
# complement of the index, so bits 0xFFFFFFFF names entity index 0, the first
# entity Bevy allocates. A caller does not have to guess: `world.query` never
# lists these entities, but the ids are a short fixed sequence from 0xFFFFFFFF
# downwards.
#
# One accepted request ended the server. The README documents raw component
# mutation as trusted low-level access that "can bypass normal transitions";
# aborting the process is a different thing.
#
# Fixed in fux, against unmodified bevy: fux registers its own
# `world.despawn_entity`, which refuses an entity carrying `IsResource` with a
# typed error and otherwise hands the request to the stock handler. fux also
# logs a failed command rather than panicking on it, which contains this class
# on its own even without the guard.
#
# Usage: 004-despawning-a-resource-entity-aborts.sh /path/to/fux
# Exit 0: reproduced (the server aborted).
# Exit 1: verified not reproduced (the server still answers).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 despawns an ordinary spawned entity instead, which must
# leave the server answering (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-004.XXXXXX)" || exit 2
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

target = None
if CONTROL:
    target = rpc("world.spawn_entity", {"components": {}})["result"]["entity"]
    print("negative control: despawning an ordinary entity %d" % target)
else:
    target = 0xFFFFFFFF
    print("despawning bits 0x%X (entity index 0, a Bevy resource entity)" % target)
try:
    reply = rpc("world.despawn_entity", {"entity": target})
    print("   reply: %s" % json.dumps(reply)[:120])
except Exception as e:
    print("   request failed at the transport: %s" % e)
time.sleep(1.5)
try:
    rpc("rpc.discover", timeout=3.0)
except Exception as e:
    print("REPRODUCED: the server stopped answering after the despawn (%s)" % e)
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
  20) echo "NOT reproduced: the server absorbed the despawn and still answers"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
