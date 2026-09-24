#!/bin/sh
# BRP policy work, finding 022 (class 6: a request fails without saying why).
#
# Some stock BRP handlers panic on requests a client can simply send. Bevy
# 0.20 catches a panicking system, logs "System panicked" and carries on, so
# the server survives -- but the client's answer is
# `receiving from an empty and closed channel`, not an error naming the
# problem, and a batch can be left half-applied. Three routes, all tried:
#   - world.mutate_components on a relationship component (ChildOf, Focused,
#     Viewing, OnTab, PaneView): they are immutable, and the stock handler
#     calls reflect_mut regardless;
#   - world.reparent_entities naming an entity that does not exist;
#   - world.trigger_event of fux::control::Control with a null value, which
#     from_reflect_with_fallback cannot build.
#
# Exit 0: reproduced (any route answered with the closed-channel error).
# Exit 1: verified not reproduced (each was answered with a real error).
# Exit 2: setup failure.
# NEGATIVE_CONTROL=1 sends the same three methods with valid requests.
#
# Usage: 022-a-panicking-brp-request-answers-a-closed-channel.sh /path/to/fux
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-022.XXXXXX)" || exit 2
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

focused = component(viewer, "fux::model::Focused")
tab = component(viewer, "fux::model::OnTab")
if focused is None or tab is None:
    print("setup: no focused pane", file=sys.stderr); sys.exit(2)
if CONTROL:
    requests = [
        ("world.mutate_components", {"entity": viewer, "component": "fux::model::Viewer", "path": ".zoom", "value": False}),
        ("world.reparent_entities", {"entities": [focused], "parent": component(focused, "bevy_ecs::hierarchy::ChildOf")}),
        ("world.trigger_event", {"event": "fux::control::Control", "value": {"viewer": viewer, "command": {"kind": "zoom"}}}),
    ]
else:
    requests = [
        ("world.mutate_components", {"entity": viewer, "component": "fux::model::Focused", "path": "", "value": focused}),
        ("world.reparent_entities", {"entities": [focused, 6450128621605086059], "parent": tab}),
        ("world.trigger_event", {"event": "fux::control::Control", "value": None}),
    ]
closed = []
for method, params in requests:
    answer = rpc(method, params)
    text = json.dumps(answer.get("error")) if "error" in answer else "ok"
    print("%s -> %s" % (method, text[:140]))
    if "empty and closed channel" in text:
        closed.append(method)
if closed:
    print("REPRODUCED: answered with a closed channel: %s" % ", ".join(closed))
    sys.exit(10)
print("every request got a real answer")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
