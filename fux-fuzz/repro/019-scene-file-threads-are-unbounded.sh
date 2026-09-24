#!/bin/sh
# Hunt 8, finding 019 (class 1: a thread panics; class 5: threads grow).
#
# Finding 018's fix gave each scene load or save its own thread, so a slow file
# no longer blocks the I/O pool. But nothing bounded those threads: a read of a
# named pipe nobody writes never ends, and each such `load_layout` kept one
# more thread for the life of the server. At the OS's thread limit --
# RLIMIT_NPROC, a pids cgroup, systemd's TasksMax -- `std::thread::spawn`
# panicked inside the pool task, and `scene_completions` then panicked on the
# main thread every update polling the dead task ("Task polled after
# completion"). Seen in a Linux container with `--pids-limit 512`.
#
# This script measures the cause, which is the same on every platform: it
# fires 64 loads, each naming an unwritten FIFO, and counts the server's
# threads. Unbounded, it gains a thread per load; bounded, at most 16.
#
# Usage: 019-scene-file-threads-are-unbounded.sh /path/to/fux
# Exit 0: reproduced (the server gained a thread for every load).
# Exit 1: verified not reproduced (the threads stayed bounded).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 names files that do not exist, so each load fails at once
# and its thread ends (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-019.XXXXXX)" || exit 2
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

SOCK="$DIR/s/fux.sock" DIR="$DIR" SERVER="$SPID" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, time
SOCK = os.environ["SOCK"]
DIR = os.environ["DIR"]
CONTROL = os.environ["CONTROL"] == "1"
SERVER = int(os.environ["SERVER"])

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

def threads():
    task = "/proc/%d/task" % SERVER
    if os.path.isdir(task):
        return len(os.listdir(task))
    import subprocess
    out = subprocess.run(["ps", "-M", "-p", str(SERVER)], capture_output=True, text=True).stdout
    return max(0, len(out.splitlines()) - 1)

viewer = rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"]
time.sleep(0.5)
before = threads()
LOADS = 64
for i in range(LOADS):
    if CONTROL:
        # A file that does not exist: the load fails at once, its thread ends.
        path = os.path.join(DIR, "missing-%d.ron" % i)
    else:
        path = os.path.join(DIR, "load-%d.fifo" % i)
        os.mkfifo(path)
    rpc("world.trigger_event", {"event": "fux::control::Control", "value": {
        "viewer": viewer,
        "command": {"kind": "load_layout", "workspace": workspace, "path": path, "mapping": []}}})
time.sleep(1.0)
grown = threads() - before
print("%d loads naming %s: the server gained %d threads" % (LOADS, "missing files" if CONTROL else "unwritten FIFOs", grown))
if grown >= LOADS:
    print("REPRODUCED: every pending scene read holds a thread, without bound")
    sys.exit(10)
print("scene file threads stayed bounded")
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
