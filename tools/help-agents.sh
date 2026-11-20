#!/bin/sh
# Runs every command of `fux help agents` (its lines indented by four blanks), in order, as one
# shell session in a session directory of its own; any command that fails, fails this. CI runs
# it on Linux and macOS.
# usage: tools/help-agents.sh [path to fux]   (default: target/release/fux)
set -eu
fux=$(cd "$(dirname "${1:-target/release/fux}")" && pwd)/fux
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" && ln -s "$fux" "$work/bin/fux"
"$fux" help | grep -q 'fux help agents'
"$fux" help agents | sed -n 's/^    \([^ ]\)/\1/p' > "$work/page.sh"
[ "$(wc -l < "$work/page.sh")" -ge 40 ]
unset FUXN_PANE FUXN_WS FUXN_KEYS FUXN_EDIT
(cd "$work" && PATH="$work/bin:$PATH" FUXN_DIR="$work/d" FUXN_KEYS="$work/keys" EDITOR=true sh -eux page.sh) > "$work/out" 2> "$work/trace" || {
  cat "$work/out" >&2; tail -n 5 "$work/trace" >&2; echo "help-agents: a command of the page failed" >&2; exit 1; }
# What the page says its commands print, they print; and what it started, it removed.
for said in '^exit code 3$' '^<<t1:[1-9][0-9]*>>$' ' end 1$' '^/$' '^state killed 15$' '^state exited 2$' \
  'crash is dead, though its state says running' '^refused at once$' 'col(row(ls w1) w2)' '│' '^command sh -c pwd; exec sleep 600$'; do
  grep -q -- "$said" "$work/out" || { cat "$work/out" >&2; echo "help-agents: no '$said'" >&2; exit 1; }
done
[ -d "$work/d" ] && [ ! -e "$work/d/agent-demo" ] && [ ! -e "$work/d/agent-view" ]
# (And nothing in the session but what the page says a viewer leaves.)
[ -z "$(ls -A "$work/d" | grep -v -x -e keyboard -e colours)" ]
echo "help-agents: ok ($(wc -l < "$work/page.sh" | tr -d ' ') lines run)"
