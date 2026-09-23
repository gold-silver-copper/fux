#!/bin/sh
# Hunt 8, finding 016 (class 5: unbounded growth).
#
# `load_layout` reads its scene file with `std::fs::read_to_string`, which
# reads the whole file into memory with no bound. The file is named by the
# caller, and an absolute path is accepted, so a same-user caller can point it
# at `/dev/zero` -- an endless file -- or at any huge file, and the server
# grows without limit: this script watches it pass 1 GB in a few seconds on
# the way to exhausting memory.
#
# fux already bounds the request body a caller sends over the socket
# (`MAX_BODY`, hunt 6 finding 007) for the same reason. A scene file read from
# disk is another way to make the server hold whatever a caller likes, and
# parsing a huge one also blocks every session while it runs.
#
# Usage: 016-scene-file-read-is-unbounded.sh /path/to/fux
# Exit 0: reproduced (the server's memory grew past 1 GB reading the file).
# Exit 1: verified not reproduced (the read is bounded and it is refused).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 loads a small, ordinary missing file instead, which is
# refused at once with no growth (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }
[ -r /dev/zero ] || { echo "/dev/zero is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-016.XXXXXX)" || exit 2
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

SOCK="$DIR/s/fux.sock" SPID="$SPID" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, subprocess, sys, time
SOCK = os.environ["SOCK"]
SPID = int(os.environ["SPID"])
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

def rss_mb():
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(SPID)], capture_output=True, text=True).stdout.strip()
    return int(out) // 1024 if out.isdigit() else 0

for _ in range(250):
    try:
        rpc("rpc.discover", timeout=1.0)
        break
    except Exception:
        time.sleep(0.1)
else:
    print("setup: server never answered", file=sys.stderr)
    sys.exit(2)

viewer = rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"]
workspace = rpc("world.query", {"data": {"components": ["fux::model::Workspace"]}})["result"][0]["entity"]
time.sleep(0.3)

base = rss_mb()
if base == 0:
    print("setup: could not read the server's RSS", file=sys.stderr)
    sys.exit(2)

path = "/nonexistent/tiny.scn.ron" if CONTROL else "/dev/zero"
print("baseline RSS %d MB; loading %s" % (base, path))
rpc("world.trigger_event", {"event": "fux::control::Control", "value": {
    "viewer": viewer, "command": {"kind": "load_layout", "workspace": workspace,
                                  "path": path, "mapping": []}}})

peak = base
deadline = time.time() + 15
while time.time() < deadline:
    time.sleep(0.3)
    peak = max(peak, rss_mb())
    if peak > base + 1024:
        print("REPRODUCED: RSS grew to %d MB (from %d MB) reading %s" % (peak, base, path))
        sys.exit(10)

print("peak RSS %d MB (baseline %d MB, growth %d MB)" % (peak, base, peak - base))
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
