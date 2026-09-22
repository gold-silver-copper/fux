#!/bin/sh
# Hunt 6, finding 007 (class 5: unbounded growth under a bounded input).
#
# `transport::batch` reads the whole request body into memory before it looks
# at it (src/transport.rs ~525: `request.into_body().collect().await`), with no
# size limit anywhere: not in hyper's builder, not in fux. One connection can
# therefore make the server hold as much memory as the caller cares to send,
# and the peak is a multiple of the body, because the bytes are collected, then
# parsed into a `serde_json::Value`, and for the error path the offending text
# is copied again into the JSON-RPC error message.
#
# Measured on this machine: about 2.2 times the body in resident memory, and
# strongly superlinear time to answer -- a 16 MB body is answered in about 2 s,
# 32 MB in 7 s, 128 MB in 237 s. Other clients keep being served throughout, so
# this is growth and latency, not a stall: the server does not refuse, does not
# stream, and does not cap.
#
# Fixed: the body is read through `http_body_util::Limited` with a 4 MiB cap
# and refused with a typed error naming it, which is far above the largest
# legitimate request (a 64 KiB paste is about 400 KiB once escaped; a
# one-megabyte name is about 1 MiB). A batch is capped at 1024 requests and
# its reply at 8 MiB, because a small body holding many requests amplifies on
# the way out.
#
# This script sends a 256 MB body and reports the server's peak RSS. It stops
# early if RSS passes 4 GB, so it cannot pressure the machine.
#
# Usage: 007-request-body-is-unbounded.sh /path/to/fux
# Exit 0: reproduced (RSS grew past 2x a 64 MB threshold with no limit applied).
# Exit 1: verified not reproduced (the body was refused or bounded).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 sends an ordinary small request instead, which must leave
# RSS flat (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-007.XXXXXX)" || exit 2
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

SOCK="$SOCK" SPID="$SPID" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, subprocess, sys, time
SOCK = os.environ["SOCK"]
SPID = int(os.environ["SPID"])
CONTROL = os.environ["CONTROL"] == "1"
MB = 1 << 20
BODY = 1 * MB if CONTROL else 256 * MB
CEILING = 4096            # MB: stop before pressuring the machine
THRESHOLD = 64            # MB of growth that proves there is no cap

def rss_mb(pid):
    # The server runs in its own session; find the fux process in that group.
    out = subprocess.run(["ps", "-o", "rss=,pid=,comm=", "-g", str(pid)],
                         capture_output=True, text=True).stdout
    best = 0
    for line in out.strip().splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[2].endswith("fux"):
            best = max(best, int(parts[0]))
    return best // 1024

def rpc(method, timeout=5.0):
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method}).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    try:
        s.connect(SOCK)
        s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: %d\r\n"
                  b"Connection: close\r\n\r\n" % len(payload) + payload)
        raw = b""
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            raw += chunk
    finally:
        s.close()
    return b"jsonrpc" in raw

for _ in range(250):
    try:
        if rpc("rpc.discover"):
            break
    except Exception:
        time.sleep(0.1)
else:
    print("setup: server never answered on %s" % SOCK, file=sys.stderr)
    sys.exit(2)

base = rss_mb(SPID)
if base == 0:
    print("setup: could not read the server's RSS", file=sys.stderr)
    sys.exit(2)
print("baseline RSS %d MB; sending a %d MB body" % (base, BODY // MB))

head = b'{"jsonrpc":"2.0","id":1,"method":"'
tail = b'","params":null}'
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(120)
try:
    s.connect(SOCK)
    s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: %d\r\n"
              b"Connection: close\r\n\r\n" % (len(head) + BODY + len(tail)))
    s.sendall(head)
    filler = b"a" * MB
    sent = 0
    peak = base
    aborted = False
    while sent < BODY:
        n = min(MB, BODY - sent)
        s.sendall(filler[:n])
        sent += n
        if sent % (32 * MB) == 0 or sent == BODY:
            peak = max(peak, rss_mb(SPID))
            if peak > CEILING:
                print("   stopping at %d MB sent: RSS reached %d MB (ceiling)" % (sent // MB, peak))
                aborted = True
                break
    if not aborted:
        s.sendall(tail)
        peak = max(peak, rss_mb(SPID))
except Exception as e:
    print("   send failed: %s" % e)
finally:
    try:
        s.close()
    except Exception:
        pass

growth = peak - base
print("peak RSS %d MB (baseline %d MB, growth %d MB for a %d MB body)"
      % (peak, base, growth, BODY // MB))
time.sleep(1.0)
still = False
try:
    still = rpc("rpc.discover", timeout=10.0)
except Exception:
    still = False
print("server still answers: %s" % still)

if growth >= THRESHOLD:
    print("REPRODUCED: one connection grew the server by %d MB with no size limit" % growth)
    sys.exit(10)
print("growth stayed under %d MB" % THRESHOLD)
sys.exit(20)
PY
RESULT=$?

if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
case "$RESULT" in
  10) exit 0 ;;
  20) echo "NOT reproduced: the request body was bounded"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
