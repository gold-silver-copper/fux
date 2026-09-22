#!/bin/sh
# Finding 001 (class 1: server aborts). Inserting a `Viewer` component onto a
# layout entity (a tab) makes navigation::repair treat that entity as a viewer.
# The next insert of any viewer relationship (Viewing/OnTab/Focused) on any real
# viewer queues repair, which inserts relationships onto the tab-as-viewer; those
# inserts re-fire remember_tab/remember_focus (src/navigation.rs:290-319), each of
# which calls repair_later, and the tab-as-viewer never converges. repair recurses
# until the thread overflows its stack and the process aborts, taking every
# attached session with it.
#
# Root cause: src/navigation.rs, `repair` (~357), `With<Viewer>` query assumes a
# Viewer component means a real viewer; the "insertions here cannot trigger
# another repair" comment (~355) is false for a Viewer on a layout entity.
#
# Usage: 001-viewer-on-tab-repair-recursion.sh /path/to/fux
# Exit 0 if the server crashes (bug reproduced); non-zero if it survives (fixed).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-001.XXXXXX)"
PORT=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
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

# Own session so the cleanup can kill the server's group without touching
# this script's own process group.
perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL="$DIR/default-shell" TERM=xterm-256color \
  PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --address 127.0.0.1 --port "$PORT" --config "$DIR/fux.json" \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

ENDPOINT="http://127.0.0.1:$PORT"
export ENDPOINT PORT DIR

python3 - <<'PY'
import json, os, time, urllib.request, sys
endpoint = os.environ["ENDPOINT"]
def rpc(method, params=None, timeout=5.0):
    body = {"jsonrpc":"2.0","id":1,"method":method}
    if params is not None: body["params"]=params
    req = urllib.request.Request(endpoint, data=json.dumps(body).encode(),
                                 headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())
# wait for readiness
for _ in range(250):
    try: rpc("rpc.discover", timeout=1.0); break
    except Exception: time.sleep(0.1)
else:
    print("server never became ready", file=sys.stderr); sys.exit(3)
viewer = rpc("fux.attach", {"rows":24,"cols":80})["result"]["viewer"]
time.sleep(0.6)
def ids(comp): return [r["entity"] for r in rpc("world.query",{"data":{"components":[comp]}})["result"]]
tab = ids("fux::model::Tab")[0]
ws  = ids("fux::model::Workspace")[0]
# 1) Viewer component onto the tab entity
rpc("world.insert_components", {"entity":tab,
    "components":{"fux::model::Viewer":{"rows":24,"cols":80,"zoom":False,"scrollback":0,"notice":None}}})
time.sleep(0.3)
# 2) any viewer relationship on the real viewer -> queues repair -> recursion
try:
    rpc("world.insert_components", {"entity":viewer,
        "components":{"fux::model::Viewing":ws}}, timeout=4.0)
except Exception:
    pass  # transport error is expected: the server is aborting
sys.exit(0)
PY

sleep 1.5
# Reproduced iff the server process is gone or its stderr shows the abort.
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
echo "NOT reproduced: server still alive (bug appears fixed)"
exit 1
