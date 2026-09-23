#!/bin/sh
# Hunt 7, finding 012 (class 2: a request hangs), Linux.
#
# Hunt 6 finding 006 left the accept loop able to make progress under
# descriptor pressure: it spends a reserved descriptor to accept and close one
# waiting connection, so the backlog drains and a client fails promptly
# instead of waiting on a listener that cannot answer. It does that once per
# 50 ms tick:
#
#   if let Some(held) = spare.take() {
#       drop(held);
#       if let Ok((shed, _)) = listener.get_ref().accept() { drop(shed); }
#       spare = reserve();
#   }
#   Timer::after(Duration::from_millis(50)).await;
#
# One connection per tick is enough on macOS, which refuses most connections
# at `connect` time, so the queue a new client joins is short. Linux accepts
# them into the listener's backlog (128 by default) and hands them out in
# order, so a new client is behind up to 128 others and waits one tick each:
# about 6.4 s, which is what this measures.
#
# The 006 fix is not broken. Its logging, its reporting interval and its
# recovery all work here, and every client after the first fails in about
# 10 ms. What this script isolates is the *first* client's wait, which 006's
# script reports as its slowest sample without distinguishing the cause.
#
# A fix is to drain the connections already waiting on each tick, bounded by
# how many there are, so the wait is one tick rather than one tick per queued
# connection.
#
# Usage: 012-descriptor-shedding-drains-one-connection-a-tick.sh /path/to/fux
# Exit 0: reproduced (a client waited more than five seconds).
# Exit 1: verified not reproduced (it failed promptly).
# Exit 2: setup failure, or not this platform.
#
# This is Linux-only, so the script refuses to judge anywhere else rather than
# reporting a pass fux has not earned. On macOS the same measurement gives
# about 3.0 s, which is better but still not prompt; BREAKS.md records it.
#
# NEGATIVE_CONTROL=1 opens and immediately closes the same connections, so
# there is no pressure, so every client is answered at once (exit 1).
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

case "$(uname -s)" in
  Linux) ;;
  *)
    echo "this finding is Linux-only; $(uname -s) queues fewer connections" >&2
    exit 2 ;;
esac

DIR="$(mktemp -d /tmp/fux-repro-012.XXXXXX)" || exit 2

cleanup() {
  [ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null
  [ -n "${SPGID:-}" ] && kill -9 -"$SPGID" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

cat > "$DIR/fux.json" <<'JSON'
{"shell":["/bin/sh","-c","printf 'PANE\n'; exec cat"]}
JSON

# The limit applies only to this subshell and the server it execs; the calling
# shell's limits are untouched.
(cd "$DIR" && ulimit -n 64 && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" TERM=xterm-256color \
  "$FUX" server --socket "$DIR/s/fux.sock" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$DIR/s/fux.sock" ERR="$DIR/err" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, time
SOCK, ERR = os.environ["SOCK"], os.environ["ERR"]
CONTROL = os.environ["CONTROL"] == "1"

def one_request(timeout):
    """Returns (answered, seconds until the socket resolved either way)."""
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "rpc.discover"}).encode()
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    started = time.time()
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
        return b"jsonrpc" in raw, time.time() - started
    except Exception:
        return False, time.time() - started
    finally:
        s.close()

for _ in range(250):
    answered, _ = one_request(1.0)
    if answered:
        break
    time.sleep(0.1)
else:
    print("setup: server never answered on %s" % SOCK, file=sys.stderr)
    sys.exit(2)
print("server ready under ulimit -n 64")

held = []
refused = 0
for _ in range(400):
    try:
        c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        c.settimeout(3)
        c.connect(SOCK)
        c.sendall(b"GET /hold HTTP/1.1\r\nHost: fux\r\n")   # a request that never ends
        if CONTROL:
            c.close()
        else:
            held.append(c)
        time.sleep(0.005)
    except Exception:
        refused += 1
        if refused > 30:
            break
print("%d connections %s (%d refused)"
      % (len(held) or refused, "held open" if not CONTROL else "opened and closed", refused))

# The measurement: fresh clients over a window, and the worst wait among them.
# A client that arrives when the backlog is completely full is refused at
# `connect` at once; the slow case is the one that gets *into* the queue and
# then waits its turn, one shed connection per tick. Sampling finds it.
slowest = 0.0
answered_any = False
samples = []
deadline = time.time() + 14
while time.time() < deadline:
    answered, waited = one_request(10.0)
    answered_any = answered_any or answered
    slowest = max(slowest, waited)
    samples.append(waited)
    time.sleep(0.25)
print("   %d clients tried; answered=%s; slowest wait %.2fs"
      % (len(samples), answered_any, slowest))
print("   waits over 1s: %d of %d" % (sum(1 for w in samples if w > 1.0), len(samples)))
waited = slowest
answered = answered_any

for c in held:
    c.close()

try:
    log = open(ERR, "rb").read().decode("utf8", "replace")
except Exception:
    log = ""
print("   log lines about accept: %d" % log.count("BRP socket accept"))

if waited > 5.0:
    print("REPRODUCED: a client waited %.1fs on a listener that could not answer, "
          "one 50 ms tick per queued connection" % waited)
    sys.exit(10)
print("NOT reproduced: the slowest client resolved in %.2fs" % waited)
sys.exit(20)
PY
RESULT=$?

case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  2) exit 2 ;;
  *) echo "probe failed with status $RESULT" >&2; exit 2 ;;
esac
