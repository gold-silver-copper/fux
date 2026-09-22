#!/bin/sh
# Finding 002 (class 8: an input a web page can send). fux used to serve BRP
# over HTTP on 127.0.0.1:15702 and checked neither Content-Type nor Origin. A
# browser "simple request" (a text/plain POST with any Origin) gets no CORS
# preflight, so it was delivered and executed: any web page the user visited
# could split a pane running an arbitrary program. Any other local user could
# also connect to the port.
#
# Fixed by removing reachability: fux now serves only on a Unix domain socket
# in a private (0700) directory, with mode 0600, and opens no internet socket
# at all. A web page cannot open a Unix socket. See fux-fuzz/BREAKS.md.
#
# The attack is a browser request, which only a TCP listener can receive, so
# this script never sends it over the Unix socket (whoever can open that
# socket is authorized by design). It asks lsof which internet sockets the
# server's own process holds, and fires the original cross-origin simple
# request at every TCP listener it finds. It never probes a port the server
# does not own, including 15702.
#
# Usage: 002-cross-origin-web-page-rce.sh /path/to/fux
# Exit 0: reproduced: a cross-origin simple request to a TCP listener of the
#         server executed a program (the marker file appeared).
# Exit 1: verified not reproduced: the server answered over its socket and
#         holds no internet listener.
# Exit 2: setup or infrastructure failure (including lsof being unable to
#         see the server's own socket); says nothing about the finding.
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
python3 -c pass 2>/dev/null || { echo "python3 is required" >&2; exit 2; }
[ -x /usr/sbin/lsof ] || command -v lsof >/dev/null 2>&1 || { echo "lsof is required" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-002.XXXXXX)" || exit 2
SOCK="$DIR/s/fux.sock"
MARKER="$DIR/PWNED-BY-A-WEB-PAGE"
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

export SOCK
# Readiness and a viewer over the socket; prints the viewer id.
VIEWER=$(python3 - <<'PY'
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
    print(rpc("fux.attach", {"rows": 24, "cols": 80})["result"]["viewer"])
except Exception as e:
    print("setup: %s" % e, file=sys.stderr)
    sys.exit(2)
PY
) || { echo "setup failure; server stderr:"; tail -4 "$DIR/err"; exit 2; }
sleep 0.6

# lsof must see the server's own Unix socket, or an empty answer below would
# be a blind tool rather than evidence.
if ! lsof -nP -a -p "$SPID" -U 2>/dev/null | grep -qF "$SOCK"; then
  echo "setup: lsof cannot see the server's own socket (pid $SPID)"
  exit 2
fi
INET=$(lsof -nP -a -p "$SPID" -i 2>/dev/null)
PORTS=$(lsof -nP -a -p "$SPID" -iTCP -sTCP:LISTEN -Fn 2>/dev/null | sed -n 's/^n.*:\([0-9][0-9]*\)$/\1/p' | sort -u)
echo "internet sockets held by the server (pid $SPID):"
echo "${INET:-  none}"

for PORT in $PORTS; do
  export PORT VIEWER MARKER
  python3 - <<'PY'
import json, os, socket
port = int(os.environ["PORT"]); viewer = int(os.environ["VIEWER"]); marker = os.environ["MARKER"]
# A browser "simple request": text/plain body, an arbitrary Origin, no preflight.
payload = json.dumps({"jsonrpc": "2.0", "id": 9, "method": "world.trigger_event",
  "params": {"event": "fux::control::Control", "value": {"viewer": viewer,
    "command": {"kind": "split", "axis": "horizontal", "program": "touch %s; sleep 30" % marker}}}}).encode()
head = ("POST / HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nOrigin: https://evil.example\r\n"
        "Content-Type: text/plain;charset=UTF-8\r\nContent-Length: %d\r\n\r\n" % (port, len(payload)))
for family, address in ((socket.AF_INET, ("127.0.0.1", port)), (socket.AF_INET6, ("::1", port))):
    try:
        s = socket.socket(family, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(address)
        s.sendall(head.encode() + payload)
        s.recv(65536)
        s.close()
        print("sent the cross-origin simple request to port %d" % port)
        break
    except OSError:
        continue
PY
done

sleep 2
if [ -e "$MARKER" ]; then
  echo "REPRODUCED: a cross-origin simple request executed a program (created $MARKER)"
  exit 0
fi
if [ -n "$PORTS" ]; then
  echo "NOT reproduced by the request, but the server listens on TCP port(s): $PORTS"
  exit 2
fi
if ! kill -0 "$SPID" 2>/dev/null; then
  echo "setup failure: the server exited"; tail -4 "$DIR/err"
  exit 2
fi
echo "NOT reproduced: the server holds no internet listener; it serves only on $SOCK"
exit 1
