#!/bin/sh
# Finding F1: a partial component payload panics the whole fux server.
#
# An agent read `fux::model::Viewer`, wrote it back without the `notice` field,
# and the server process died, taking every PTY in the session with it. This
# script reproduces that with no agent and no harness involved.
#
#   ./evidence/partial-component-panic.sh ../target/release/fux 17771
#
# Expected: the complete five-field payload is accepted and the server stays up;
# the same payload minus `notice` kills it. Observed deterministically, 3 of 3.
set -u

BIN="${1:?usage: partial-component-panic.sh PATH_TO_FUX PORT}"
PORT="${2:?usage: partial-component-panic.sh PATH_TO_FUX PORT}"

DIR=$(mktemp -d "${TMPDIR:-/tmp}/fux-f1.XXXXXX")
printf '{"shell":["/bin/sh"],"history_lines":100}' > "$DIR/fux.json"

# A minimal environment: nothing credential-shaped reaches the server or its children.
env -i PATH=/usr/bin:/bin HOME="$DIR" SHELL=/bin/sh PS1='$ ' \
  "$BIN" server --port "$PORT" --config "$DIR/fux.json" > "$DIR/server.log" 2>&1 &
SERVER=$!
sleep 1.5

rpc() {
  curl -sS -m 5 -X POST "http://127.0.0.1:$PORT" \
    -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" 2>&1
}

alive() {
  if rpc rpc.discover null 2>/dev/null | grep -q jsonrpc; then echo "SERVER ALIVE"; else echo "SERVER DEAD"; fi
}

VIEWER=$(rpc fux.attach '{"rows":24,"cols":80}' |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["viewer"])')
echo "viewer entity: $VIEWER"

echo
echo "1. complete payload (all five Viewer fields)"
rpc world.insert_components \
  "{\"entity\":$VIEWER,\"components\":{\"fux::model::Viewer\":{\"rows\":24,\"cols\":80,\"zoom\":false,\"scrollback\":5,\"notice\":null}}}"
echo "   -> $(alive)"

echo
echo "2. partial payload (identical, minus \`notice\`)"
rpc world.insert_components \
  "{\"entity\":$VIEWER,\"components\":{\"fux::model::Viewer\":{\"rows\":24,\"cols\":80,\"zoom\":false,\"scrollback\":5}}}"
echo "   -> $(alive)"

sleep 0.5
echo
if kill -0 "$SERVER" 2>/dev/null; then echo "server process: still running"; else echo "server process: gone"; fi
echo "server log tail:"
tail -4 "$DIR/server.log" | sed 's/^/   /'

kill -9 "$SERVER" 2>/dev/null
wait "$SERVER" 2>/dev/null
rm -rf "$DIR"
