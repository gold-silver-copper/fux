#!/bin/sh
# The example from the README, run for real: CI runs it on Linux and macOS.
# usage: ./example.sh [path to fux3]   (default: target/release/fux3)
set -eu
fux3=${1:-target/release/fux3}
FUX3C_DIR=$(mktemp -d) && export FUX3C_DIR
trap 'rm -rf "$FUX3C_DIR"' EXIT

# Waits up to 10 s until the command succeeds.
until_ok() { n=0; until "$@" >/dev/null 2>&1; do n=$((n + 1)); [ $n -lt 200 ] || { echo "timed out: $*" >&2; exit 1; }; sleep 0.05; done; }

D=$("$fux3" dir)
"$fux3" start build -- sh                       # a pane is a directory kept by one small process
printf 'echo h""i\n' > "$D/main/build/in"       # plain tools are the clients
until_ok grep -qx hi "$D/main/build/screen"
cat "$D/main/build/screen"
ls "$D" "$D/main"                               # workspaces are folders, panes are folders in them
[ "$(ls "$D")" = main ] && [ "$(ls "$D/main")" = build ]
grep -q '^state running' "$D/main/build/status"
echo kill > "$D/main/build/ctl"                 # hangs up the program
"$fux3" wait "$D/main/build" | grep '^state'
echo "example: ok"
