#!/bin/sh
# Finding 001 (class 1: server abort). Two BRP requests, both accepted, used
# to abort the server with a stack overflow:
#   1. world.insert_components a fux::model::Viewer onto a TAB entity;
#   2. world.insert_components any viewer relationship (Viewing, OnTab,
#      Focused) onto a real viewer, which queues navigation::repair.
# repair treated the tab as a viewer, gave it relationships whose hooks queued
# repair again, and recursed until the stack overflowed.
#
# Fixed: a Viewer inserted onto a layout node is removed (the layout role
# wins), passes over viewers select only genuine viewers, and repair's
# scheduling is bounded. See fux-fuzz/BREAKS.md.
#
# The server is reached over its Unix domain socket, the only transport.
#
# Usage: 001-viewer-on-tab-repair-recursion.sh /path/to/fux
# Exit 0: reproduced (the server aborted, stopped answering, or kept the
#         Viewer on the tab).
# Exit 1: verified not reproduced: the server answered before and after the
#         trigger and the tab no longer carries a Viewer.
# Exit 2: setup or infrastructure failure; says nothing about the finding.
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-001.XXXXXX)" || exit 2
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

# Own session, in the fixture directory, so cleanup can kill the server's
# group without touching this script's own, and nothing lands in the caller's
# working directory.
(cd "$DIR" && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL="$DIR/default-shell" TERM=xterm-256color \
  PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --socket "$SOCK" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

export SOCK
python3 - <<'PY'
import json, os, socket, sys, time
SOCK = os.environ["SOCK"]

def rpc(method, params=None, timeout=5.0):
    body = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        body["params"] = params
    data = json.dumps(body).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(SOCK)
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\n"
                  b"Content-Length: %d\r\nConnection: close\r\n\r\n" % len(data) + data)
        raw = b""
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            raw += chunk
    finally:
        s.close()
    head, _, payload = raw.partition(b"\r\n\r\n")
    if b"transfer-encoding: chunked" in head.lower():
        decoded = b""
        while payload:
            size, _, rest = payload.partition(b"\r\n")
            n = int(size, 16)
            if n == 0:
                break
            decoded += rest[:n]
            payload = rest[n + 2:]
        payload = decoded
    return json.loads(payload.decode())

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
    def ids(comp):
        return [r["entity"] for r in rpc("world.query", {"data": {"components": [comp]}})["result"]]
    tab = ids("fux::model::Tab")[0]
    ws = ids("fux::model::Workspace")[0]
except Exception as e:
    print("setup: %s" % e, file=sys.stderr)
    sys.exit(2)
# 1) A Viewer component onto the tab entity.
try:
    rpc("world.insert_components", {"entity": tab, "components": {
        "fux::model::Viewer": {"rows": 24, "cols": 80, "zoom": False, "scrollback": 0, "notice": None}}})
except Exception:
    pass
time.sleep(0.3)
# 2) Any viewer relationship on the real viewer queues repair.
try:
    rpc("world.insert_components", {"entity": viewer, "components": {"fux::model::Viewing": ws}}, timeout=4.0)
except Exception:
    pass  # a vulnerable server aborts here
time.sleep(1.5)
# The server must answer a fresh request, and the tab must have lost its Viewer.
try:
    rpc("rpc.discover", timeout=3.0)
except Exception as e:
    print("no answer after the trigger: %s" % e)
    sys.exit(10)
try:
    left = rpc("world.get_components", {"entity": tab, "components": ["fux::model::Viewer"]})
    kept = "fux::model::Viewer" in left["result"]["components"]
except Exception as e:
    print("no answer after the trigger: %s" % e)
    sys.exit(10)
if kept:
    print("the tab still carries a Viewer component")
    sys.exit(11)
print("server answered after the trigger; tab %d carries no Viewer" % tab)
sys.exit(20)
PY
RESULT=$?

# The client failed before the trigger: nothing is known about the finding.
if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
sleep 0.2
if grep -q "overflowed its stack\|stack overflow\|panicked at" "$DIR/err" 2>/dev/null; then
  echo "REPRODUCED: server aborted"
  echo "--- stderr tail ---"; tail -4 "$DIR/err"
  exit 0
fi
if ! kill -0 "$SPID" 2>/dev/null; then
  echo "REPRODUCED: server process exited"
  tail -4 "$DIR/err"
  exit 0
fi
case "$RESULT" in
  10) echo "REPRODUCED: server alive but not answering"; exit 0 ;;
  11) echo "REPRODUCED: Viewer left on the tab entity"; exit 0 ;;
  20) echo "NOT reproduced: server healthy over its socket and the Viewer was removed"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); see stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
