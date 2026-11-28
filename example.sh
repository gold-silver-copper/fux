#!/bin/sh
# The example from the README, run for real: CI runs it on Linux and macOS.
# usage: ./example.sh [path to fux]   (default: target/release/fux)
set -eu
unset COLUMNS LINES
fux=${1:-target/release/fux}
run="" timer=""
# (A session of its own: the default one of a runtime directory made for it.)
XDG_RUNTIME_DIR=$(mktemp -d) && export XDG_RUNTIME_DIR
stop_tree() {
  [ -z "$1" ] || ps -e -o pid= -o ppid= | awk -v r="$1" '{p[$1]=$2} function t(x,i){print x; for(i in p) if(p[i]==x&&i!=x) t(i)} END{t(r)}' | xargs kill -KILL 2>/dev/null || :
}
cleanup() {
  stop_tree "$run"; stop_tree "$timer"
  for status in "$XDG_RUNTIME_DIR"/fux/*/*/status; do
    [ -f "$status" ] || continue
    p=$(sed -n 's/^pid //p' "$status"); [ -z "$p" ] || kill -KILL -"$p" 2>/dev/null || :
  done
  rm -rf "$XDG_RUNTIME_DIR"
}
trap cleanup EXIT; trap 'exit 130' HUP INT TERM

# Waits up to 10 s until the command succeeds.
until_ok() { n=0; until "$@" >/dev/null 2>&1; do n=$((n + 1)); [ $n -lt 200 ] || { echo "timed out: $*" >&2; exit 1; }; sleep 0.05; done; }

(
  D=$("$fux" dir)
  "$fux" start build -- sh                       # a pane is a directory kept by one small process
  printf 'echo h""i\n' > "$D/main/build/in"       # plain tools are the clients
  until_ok grep -qx hi "$D/main/build/shown/screen"
  cat "$D/main/build/shown/screen"
  ls "$D" "$D/main"                               # workspaces are folders, panes are folders in them
  [ "$(ls "$D")" = main ]
  [ "$(ls "$D/main")" = build ]
  grep -qx running "$D/main/build/state"           # every fact about a pane is a small file
  echo kill > "$D/main/build/ctl"                 # hangs up the program
  "$fux" wait "$D/main/build" | grep -E '^state (killed 1$|exited [0-9][0-9]*$)'
) & run=$!
(n=0; while kill -0 "$run" 2>/dev/null && [ "$n" -lt 15 ]; do sleep 1; n=$((n + 1)); done
  ! kill -0 "$run" 2>/dev/null || { : > "$XDG_RUNTIME_DIR/timed_out"; echo "example: timed out" >&2; stop_tree "$run"; }) & timer=$!
rc=0; wait "$run" || rc=$?; stop_tree "$timer"; wait "$timer" 2>/dev/null || :
[ ! -e "$XDG_RUNTIME_DIR/timed_out" ] || exit 1
[ "$rc" -eq 0 ] || exit "$rc"
echo "example: ok"
