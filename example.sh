#!/bin/sh
# The example from the README, run for real: CI runs it on Linux and macOS.
# usage: ./example.sh [path to fux]   (default: target/release/fux)
set -eu
fux=${1:-target/release/fux}
FUXN_DIR=$(mktemp -d) && export FUXN_DIR
unset FUXN_PANE FUXN_WS    # run in a pane, `build` would else start in that pane's workspace
trap 'rm -rf "$FUXN_DIR"' EXIT

# Waits up to 10 s until the command succeeds.
until_ok() { n=0; until "$@" >/dev/null 2>&1; do n=$((n + 1)); [ $n -lt 200 ] || { echo "timed out: $*" >&2; exit 1; }; sleep 0.05; done; }

D=$("$fux" dir)
"$fux" start build -- sh                       # a pane is a directory kept by one small process
printf 'echo h""i\n' > "$D/main/build/in"       # plain tools are the clients
until_ok grep -qx hi "$D/main/build/screen"
cat "$D/main/build/screen"
ls "$D" "$D/main"                               # workspaces are folders, panes are folders in them
[ "$(ls "$D")" = main ] && [ "$(ls "$D/main")" = build ]
grep -q '^state running' "$D/main/build/status"
echo kill > "$D/main/build/ctl"                 # hangs up the program
"$fux" wait "$D/main/build" | grep '^state'
echo "example: ok"
