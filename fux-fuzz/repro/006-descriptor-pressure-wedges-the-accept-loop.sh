#!/bin/sh
# Hunt 6, finding 006 (class 2: the server stops answering, and the condition
# does not clear on its own).
#
# `transport::serve` used to treat EMFILE and ENFILE as transient, alongside
# ECONNABORTED, EINTR and EAGAIN: warn, sleep 50 ms, loop. An aborted peer is
# momentary and retrying is right; a descriptor shortage persists until
# something frees a descriptor, and retrying the same accept cannot free one.
# So while one same-user process held connections open, the server logged about
# twenty warnings a second for as long as it lasted -- unbounded growth on a
# server whose stderr is redirected to a file -- and every new client waited in
# silence on a listener that never answered, because the backlog stayed full.
#
# Fixed: the two kinds of error are now distinguished. Under descriptor
# pressure fux reports the condition once at error level and then at most every
# five seconds, spends a reserved descriptor to accept and immediately close one
# waiting connection so the backlog drains, and reports recovery when accept
# succeeds again.
#
# What that does and does not buy, stated plainly: a server with no descriptors
# cannot serve a new client, and no change here alters that. What changed is
# that a client now fails at once instead of hanging, the log is bounded, the
# condition is visible, and the server recovers by itself the moment the
# pressure clears.
#
# This script therefore measures the fixed behaviour rather than mere
# reachability. The server runs under `ulimit -n 64` so the case is bounded,
# quick and cannot disturb the machine; the same handling applies at any limit.
#
# Usage: 006-descriptor-pressure-wedges-the-accept-loop.sh /path/to/fux
# Exit 0: reproduced -- the log flooded, or a client hung, or the server did
#         not recover once the pressure cleared.
# Exit 1: verified not reproduced -- bounded logging, prompt failures, the
#         condition reported, and automatic recovery.
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 opens and immediately closes the same connections instead
# of holding them, so no pressure ever builds; that must also exit 1.
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-006.XXXXXX)" || exit 2
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

# The limit applies only to this subshell and the server it execs; the calling
# shell's limits are untouched.
(cd "$DIR" && ulimit -n 64 && exec perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL="$DIR/default-shell" TERM=xterm-256color \
  PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --socket "$SOCK" --config "$DIR/fux.json") \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

SOCK="$SOCK" ERR="$DIR/err" CONTROL="${NEGATIVE_CONTROL:-0}" python3 - <<'PY'
import json, os, socket, sys, time
SOCK = os.environ["SOCK"]
ERR = os.environ["ERR"]
CONTROL = os.environ["CONTROL"] == "1"

def rpc(method, timeout=2.0):
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
print("server ready under ulimit -n 64")

held = []
refused = 0
for i in range(400):
    try:
        c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        c.settimeout(3)
        c.connect(SOCK)
        c.sendall(b"GET /hold HTTP/1.1\r\nHost: fux\r\n")    # a request that never ends
        if CONTROL:
            c.close()
        else:
            held.append(c)
        time.sleep(0.005)
    except Exception:
        refused += 1
        if refused > 30:
            break
print("opened %d connections (%d refusals), holding=%s" % (len(held) or i, refused, not CONTROL))

# Under pressure: does a new client fail promptly, or hang?
slowest = 0.0
attempts = 0
answered = 0
start = time.time()
while time.time() - start < 12:
    attempts += 1
    t0 = time.time()
    try:
        if rpc("rpc.discover", timeout=8.0):
            answered += 1
    except Exception:
        pass
    slowest = max(slowest, time.time() - t0)
    time.sleep(0.25)

def log_text():
    try:
        with open(ERR, "rb") as f:
            return f.read().decode("utf8", "replace")
    except Exception:
        return ""

text = log_text()
reports = text.count("out of descriptors")
lines = text.count("BRP socket accept")
print("attempts=%d answered=%d slowest=%.2fs; log lines=%d, condition reports=%d"
      % (attempts, answered, slowest, lines, reports))

# Release the pressure: the server must come back on its own.
for c in held:
    c.close()
recovered = None
t0 = time.time()
while time.time() - t0 < 20:
    try:
        if rpc("rpc.discover", timeout=2.0):
            recovered = time.time() - t0
            break
    except Exception:
        pass
    time.sleep(0.1)
text = log_text()
print("recovered=%s, recovery reported=%d"
      % (("%.1fs" % recovered) if recovered is not None else "no",
         text.count("accept recovered")))

if CONTROL:
    # No pressure was ever applied, so the server simply keeps working.
    if answered > 0 and lines == 0:
        print("NOT reproduced: no pressure, the server answered throughout")
        sys.exit(20)
    print("REPRODUCED: the server misbehaved with no pressure applied")
    sys.exit(10)

if lines > 40:
    print("REPRODUCED: the accept loop logged %d lines, a flood" % lines)
    sys.exit(10)
if slowest > 5.0:
    print("REPRODUCED: a client waited %.1fs on a listener that could not answer" % slowest)
    sys.exit(10)
if reports == 0:
    print("REPRODUCED: descriptor pressure was never reported")
    sys.exit(10)
if recovered is None:
    print("REPRODUCED: the server did not recover after the pressure cleared")
    sys.exit(10)
print("NOT reproduced: %d log lines (not a flood), slowest client failure %.2fs "
      "(prompt), pressure reported %d time(s), recovered in %.1fs"
      % (lines, slowest, reports, recovered))
sys.exit(20)
PY
RESULT=$?

if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
case "$RESULT" in
  10) exit 0 ;;
  20) exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
