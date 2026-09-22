#!/bin/sh
# Hunt 6, finding 003 (class 1 abort, class 8 refused-input-changes-state,
# class 6 silent invariant break).
#
# A `fux.frame+watch` request names the viewer to stream. fux registers the
# "this watcher went away, detach its viewer" bookkeeping straight from the
# request's params, before the request is dispatched and without checking that
# the entity is a viewer at all (src/main.rs ~191-208). When the response
# channel closes, `server::disconnected` (src/server.rs ~450) despawns that
# entity: `commands.entity(entity).try_despawn()`.
#
# So the id in a watch request is a despawn of the caller's choosing:
#   * name a tab, and the tab (with its panes) is despawned;
#   * name a process entity, and the child process is killed;
#   * name one of Bevy 0.19's resource entities, and despawning it aborts the
#     whole server, taking every session and child process with it.
#
# The request does not even have to be accepted. A batch body is refused with
# "Streaming can not be used in batch requests", and the detach still fires,
# because the registration happens before dispatch. That is one ordinary POST.
#
# `Entity::to_bits` is an opaque encoding whose low 32 bits are the bitwise
# complement of the index, so bits 0xFFFFFFFF is entity index 0, 0xFFFFFFFE is
# index 1, and so on: the first entities Bevy allocates, which are resources.
#
# This script drives the fatal variant: one batch POST naming entity index 0.
#
# Usage: 003-watch-close-despawns-any-entity.sh /path/to/fux
# Exit 0: reproduced (the server aborted, or a named tab was despawned).
# Exit 1: verified not reproduced (the server survived and kept the tab).
# Exit 2: setup or infrastructure failure; says nothing about the finding.
#
# NEGATIVE_CONTROL=1 sends the same request without `+watch` in the method,
# which must leave the server and the tab alone (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-003.XXXXXX)" || exit 2
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
METHOD = "fux.frame" if CONTROL else "fux.frame+watch"

def http(payload, timeout=6.0):
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
    head, _, body = raw.partition(b"\r\n\r\n")
    if b"transfer-encoding: chunked" in head.lower():
        out = b""
        while body:
            size, _, rest = body.partition(b"\r\n")
            try:
                n = int(size, 16)
            except ValueError:
                break
            if n == 0:
                break
            out += rest[:n]
            body = rest[n + 2:]
        body = out
    return json.loads(body.decode())

def rpc(method, params=None, timeout=6.0):
    b = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        b["params"] = params
    return http(json.dumps(b).encode(), timeout)

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
    viewer = rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"]
    time.sleep(0.6)
    rpc("world.trigger_event", {"event": "fux::control::Control",
        "value": {"viewer": viewer, "command": {"kind": "tab_new", "name": "victim"}}})
    time.sleep(0.8)
    tabs = [r["entity"] for r in rpc("world.query",
            {"data": {"components": ["fux::model::Tab"]}})["result"]]
    if len(tabs) < 2:
        print("setup: expected two tabs, got %r" % tabs, file=sys.stderr)
        sys.exit(2)
    victim = max(tabs)
except Exception as e:
    print("setup: %s" % e, file=sys.stderr)
    sys.exit(2)

print("viewer %d, tabs %r, naming tab %d" % (viewer, sorted(tabs), victim))

# (1) A batch POST naming a live tab. It is refused, and the tab dies anyway.
try:
    reply = http(json.dumps([{"jsonrpc": "2.0", "id": 1, "method": METHOD,
                              "params": {"viewer": victim}}]).encode())
    print("   batch reply: %s" % json.dumps(reply)[:110])
except Exception as e:
    print("   batch request failed: %s" % e)
time.sleep(1.5)

try:
    after = [r["entity"] for r in rpc("world.query",
             {"data": {"components": ["fux::model::Tab"]}})["result"]]
except Exception as e:
    print("REPRODUCED: the server stopped answering after a refused batch request (%s)" % e)
    sys.exit(10)
if victim not in after:
    print("REPRODUCED: refused %s request despawned tab %d; tabs now %r"
          % (METHOD, victim, sorted(after)))
    sys.exit(10)
print("   tab %d survived; tabs now %r" % (victim, sorted(after)))

# (2) The fatal variant: entity index 0, which in Bevy 0.19 is a resource entity.
try:
    reply = http(json.dumps([{"jsonrpc": "2.0", "id": 1, "method": METHOD,
                              "params": {"viewer": 0xFFFFFFFF}}]).encode())
    print("   batch reply for entity index 0: %s" % json.dumps(reply)[:110])
except Exception as e:
    print("   request failed: %s" % e)
time.sleep(1.5)
try:
    rpc("rpc.discover", timeout=3.0)
except Exception as e:
    print("REPRODUCED: naming a resource entity in a %s request killed the server (%s)"
          % (METHOD, e))
    sys.exit(10)
print("server still answers after both requests")
sys.exit(20)
PY
RESULT=$?

if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
sleep 0.3
if grep -q "panicked at\|Resource entities are not supposed" "$DIR/err" 2>/dev/null; then
  echo "REPRODUCED: server panicked"
  echo "--- stderr tail ---"; grep -m3 "panicked at\|Resource entities\|Encountered" "$DIR/err"
  exit 0
fi
if ! kill -0 "$SPID" 2>/dev/null; then
  echo "REPRODUCED: server process exited"; tail -4 "$DIR/err"; exit 0
fi
case "$RESULT" in
  10) exit 0 ;;
  20) echo "NOT reproduced: the server survived and kept the entity it was told to watch"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
