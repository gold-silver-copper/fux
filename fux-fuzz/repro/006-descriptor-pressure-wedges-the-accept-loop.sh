#!/bin/sh
# Hunt 6, finding 006 (class 2: a request never returns / the server stops
# answering, and the condition does not clear on its own).
#
# When `accept` fails with EMFILE or ENFILE, `transport::serve` treats it as
# transient (src/transport.rs ~390-440): it logs a warning, sleeps 50 ms and
# loops. It never accepts-and-closes to drain the backlog and never sheds a
# connection, so while the descriptor pressure lasts:
#
#   * no new client can connect at all -- `fux attach`, `fux rpc` and `fux stop`
#     all fail, because every one of them needs a new connection;
#   * the warning repeats about twenty times a second into stderr for as long
#     as the pressure lasts, which is unbounded log growth on a server whose
#     stderr is redirected to a file.
#
# The pressure is supplied here by one same-user process holding connections
# open, which is exactly the caller the README already trusts with the whole
# API, so this is not a privilege question: it is that an ordinary client can
# make the server unreachable to every other client, and that the server does
# not recover on its own.
#
# The server is run under `ulimit -n 64` so the case is bounded, quick and
# cannot disturb the machine. The same wedge happens at any limit; the limit
# only decides how many connections it takes. macOS `launchctl limit maxfiles`
# is 256 by default, so a server started from a launchd context reaches it with
# roughly two hundred connections.
#
# Usage: 006-descriptor-pressure-wedges-the-accept-loop.sh /path/to/fux
# Exit 0: reproduced (no new client could connect for at least ten seconds
#         while the pressure lasted).
# Exit 1: verified not reproduced (a new client still connected).
# Exit 2: setup or infrastructure failure.
#
# NEGATIVE_CONTROL=1 opens and immediately closes the same connections instead
# of holding them, which must leave the server reachable (exit 1).
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

# Can any new client connect now? Measure for fifteen seconds.
start = time.time()
ok_at = None
attempts = 0
while time.time() - start < 15:
    attempts += 1
    try:
        if rpc("rpc.discover", timeout=1.0):
            ok_at = time.time() - start
            break
    except Exception:
        pass
    time.sleep(0.25)
blocked = time.time() - start

warnings = 0
try:
    with open(ERR, "rb") as f:
        warnings = f.read().decode("utf8", "replace").count("BRP socket accept")
except Exception:
    pass
print("new-client attempts=%d, first success at %s, accept warnings in stderr=%d"
      % (attempts, ("%.1fs" % ok_at) if ok_at is not None else "never", warnings))

if ok_at is None:
    # Does it clear once the pressure stops? That is the recovery half.
    for c in held:
        c.close()
    time.sleep(1.0)
    recovered = False
    t0 = time.time()
    while time.time() - t0 < 20:
        try:
            if rpc("rpc.discover", timeout=2.0):
                recovered = True
                break
        except Exception:
            time.sleep(0.2)
    print("after releasing the connections, the server answered again: %s (%.1fs)"
          % (recovered, time.time() - t0))
    print("REPRODUCED: no new client could connect for %.0fs while one process held "
          "connections open; %d accept warnings were logged" % (blocked, warnings))
    sys.exit(10)

for c in held:
    c.close()
print("a new client connected after %.1fs" % ok_at)
sys.exit(20)
PY
RESULT=$?

if [ "$RESULT" -eq 2 ]; then
  echo "setup failure; server stderr:"; tail -4 "$DIR/err"
  exit 2
fi
case "$RESULT" in
  10) exit 0 ;;
  20) echo "NOT reproduced: the server stayed reachable to new clients"; exit 1 ;;
  *) echo "setup failure (client exit $RESULT); server stderr:"; tail -4 "$DIR/err"; exit 2 ;;
esac
