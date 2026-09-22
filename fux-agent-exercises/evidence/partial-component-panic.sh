#!/bin/sh
# Finding F1: a partial component payload used to panic the whole fux server.
#
# An agent read `fux::model::Viewer`, wrote it back without the `notice` field,
# and the server process died, taking every PTY in the session with it. This
# script reproduces that request with no agent and no harness involved, and
# now documents the fixed behavior.
#
#   ./evidence/partial-component-panic.sh ../target/release/fux 17771
#
# Expected since the fix: the complete five-field payload is accepted; the same
# payload minus `notice` is accepted as well, because `notice` is an `Option`
# the published `registry.schema` does not list as required; a payload missing
# a required field (`rows`) is rejected with a JSON-RPC error naming the field;
# and the server answers after every step. Before the fix, step 2 killed it:
#
#   panicked at bevy_ecs-0.19.1/src/reflect/mod.rs:140:13:
#   Couldn't create an instance of `fux::model::Viewer` using the reflected
#   `FromReflect`, `Default` or `FromWorld` traits.
#
# Exits non-zero if the server dies or a required-field payload is accepted.
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

STATUS=0
alive() {
  if rpc rpc.discover null 2>/dev/null | grep -q jsonrpc; then echo "SERVER ALIVE"; else echo "SERVER DEAD"; STATUS=1; fi
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
echo "2. partial payload (identical, minus \`notice\`; the original F1 request)"
rpc world.insert_components \
  "{\"entity\":$VIEWER,\"components\":{\"fux::model::Viewer\":{\"rows\":24,\"cols\":80,\"zoom\":false,\"scrollback\":5}}}"
echo "   -> $(alive)"

echo
echo "3. partial payload missing a required field (\`rows\`)"
REPLY=$(rpc world.insert_components \
  "{\"entity\":$VIEWER,\"components\":{\"fux::model::Viewer\":{\"cols\":80,\"zoom\":false,\"scrollback\":5,\"notice\":null}}}")
echo "$REPLY"
case "$REPLY" in
  *'missing field `rows`'*) echo "   -> rejected, naming the field" ;;
  *) echo "   -> NOT REJECTED"; STATUS=1 ;;
esac
echo "   -> $(alive)"

sleep 0.5
echo
if kill -0 "$SERVER" 2>/dev/null; then echo "server process: still running"; else echo "server process: gone"; STATUS=1; fi
echo "server log tail:"
tail -4 "$DIR/server.log" | sed 's/^/   /'

kill -9 "$SERVER" 2>/dev/null
wait "$SERVER" 2>/dev/null
rm -rf "$DIR"
exit "$STATUS"
