#!/bin/sh
# Hunt 8, finding 018 (class 2: the server hangs).
#
# `load_layout` reads its scene file with a blocking `std::fs::read_to_string`
# (now `read_scene`) inside a task on Bevy's `IoTaskPool`. That pool is tiny on
# a small machine -- one thread when there are two cores, at most four -- and
# it also runs the BRP serving loop and every pane's PTY I/O. So a load whose
# file is slow to produce its bytes blocks a pool thread, and enough concurrent
# such loads block every pool thread, and then the server answers nothing: the
# next request hangs until the reads finish.
#
# A named pipe makes "slow to produce bytes" exact: a read of a FIFO that is
# never written blocks forever. This is the shape fux-fuzz's `race`, `churn`
# and `scene_refs` scenarios use, and why they time out on a two-core CI
# runner (`timeout: global`), on `origin/main` as well.
#
# This script opens several concurrent `load_layout` requests, each naming a
# FIFO nothing ever writes, then asks `rpc.discover`. A server whose file reads
# are off the pool answers at once; a server that reads them on the pool hangs.
#
# Usage: 018-a-blocking-scene-read-stalls-the-server.sh /path/to/fux
# Exit 0: reproduced (the server stopped answering while loads were pending).
# Exit 1: verified not reproduced (it kept answering).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 opens the same number of connections but sends a harmless
# request on each instead of a FIFO load, so the server keeps answering (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-018.XXXXXX)" || exit 2
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

SOCK="$DIR/s/fux.sock" DIR="$DIR" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, threading, time
SOCK = os.environ["SOCK"]
DIR = os.environ["DIR"]
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

workspace = None
for _ in range(250):
    try:
        result = rpc("world.query", {"data": {"components": ["fux::model::Workspace"]}}, timeout=1.0)["result"]
        if result:
            workspace = result[0]["entity"]
            break
    except Exception:
        pass
    time.sleep(0.1)
if workspace is None:
    print("setup: server never produced a workspace", file=sys.stderr)
    sys.exit(2)

# More than the IoTaskPool's ceiling (four). Attach every viewer first, while
# the pool is idle, then start the loads: each load's read blocks a pool thread
# on its unwritten FIFO.
LOADS = 8
viewers = []
for i in range(LOADS):
    if CONTROL:
        rpc("rpc.discover")
        continue
    viewers.append(rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"])

for i, viewer in enumerate(viewers):
    pipe = os.path.join(DIR, "load-%d.fifo" % i)
    os.mkfifo(pipe)
    # The load control returns after spawning the read; the read blocks on the
    # unwritten FIFO. Fire it from a thread and do not wait for a reply.
    def fire(viewer=viewer, pipe=pipe):
        try:
            rpc("world.trigger_event", {"event": "fux::control::Control", "value": {
                "viewer": viewer,
                "command": {"kind": "load_layout", "workspace": workspace, "path": pipe, "mapping": []}}},
                timeout=3.0)
        except Exception:
            pass
    threading.Thread(target=fire, daemon=True).start()
time.sleep(1.5)

# With the loads pending, can a fresh client still be served?
answered = 0
slowest = 0.0
for _ in range(3):
    start = time.time()
    try:
        rpc("rpc.discover", timeout=4.0)
        answered += 1
    except Exception:
        pass
    slowest = max(slowest, time.time() - start)
    time.sleep(0.2)

print("with %d loads pending: answered %d of 3, slowest %.2fs" % (0 if CONTROL else LOADS, answered, slowest))
if answered == 0:
    print("REPRODUCED: the server stopped answering while scene reads blocked the pool")
    sys.exit(10)
print("server kept answering")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
