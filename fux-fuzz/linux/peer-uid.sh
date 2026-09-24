#!/bin/sh
# The BRP socket serves only the server's own user: a check with a second UID.
#
# Usage: fux-fuzz/linux/peer-uid.sh ARCH
#
# Builds fux in the Linux container (fux-fuzz/linux/run.sh), then, in a root
# container, runs the server as the ordinary user `fux` and sends rpc.discover
# to it twice: as `fux`, which must be answered, and as root, which must be
# refused. Root passes every file mode, so only the peer check can stop it.
# Exit 0: both as expected. Exit 1: not. Exit 2: setup failure.
# FUX_BIN=/target/PATH runs another binary from the target volume instead of
# /target/debug/fux, e.g. one built without the check, which must give 1.
set -u
ARCH="${1:?usage: $0 arm64|amd64}"
HERE="$(cd "$(dirname "$0")" && pwd)"
"$HERE/run.sh" "$ARCH" sh -c 'cd /src && cargo build --locked' >&2 || exit 2
exec docker run --rm --init --user root --platform "linux/$ARCH" \
  -e FUX_BIN="${FUX_BIN:-/target/debug/fux}" \
  -v "fux-linux-target-$ARCH:/target" "fux-linux:$ARCH" sh -c '
set -u
D=$(mktemp -d /tmp/peer.XXXXXX) || exit 2
chmod 755 "$D"
printf "{\"shell\":[\"/bin/sh\",\"-c\",\"exec sleep 600\"]}\n" > "$D/fux.json"
cat > "$D/ask.py" <<"PY"
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(5)
data = b""
try:
    s.connect(sys.argv[1])
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "rpc.discover"}).encode()
    s.sendall(b"POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
except OSError:
    pass
print("answered" if b"Bevy Remote Protocol" in data else "refused")
PY
mkdir "$D/run" && chown fux:fux "$D/run"
setpriv --reuid=fux --regid=fux --init-groups env HOME=/home/fux \
  "$FUX_BIN" server --socket "$D/run/s/fux.sock" --config "$D/fux.json" \
  > "$D/out" 2> "$D/err" &
for _ in $(seq 100); do [ -S "$D/run/s/fux.sock" ] && break; sleep 0.1; done
[ -S "$D/run/s/fux.sock" ] || { echo "setup: the server made no socket" >&2; cat "$D/err" >&2; exit 2; }
USER_UID=$(id -u fux)
AS_USER=$(setpriv --reuid=fux --regid=fux --init-groups python3 "$D/ask.py" "$D/run/s/fux.sock")
AS_ROOT=$(python3 "$D/ask.py" "$D/run/s/fux.sock")
echo "as uid $USER_UID (the server'"'"'s): $AS_USER"
echo "as uid 0: $AS_ROOT"
grep -i "refused a brp connection" "$D/err" | sed "s/\x1b\[[0-9;]*m//g" | tail -1
if [ "$AS_USER" = answered ] && [ "$AS_ROOT" = refused ]; then exit 0; fi
exit 1
'
