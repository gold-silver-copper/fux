#!/bin/sh
# Runs every command of `fux --help` (its lines indented by four blanks), in order, as one
# shell session in a session of its own (the default one of a runtime directory made for it,
# with a config directory and a home there); any command that fails, fails this. CI runs
# it on Linux and macOS. The page's section "IF YOU HAVE PERL" is run only where perl is found
# (CI's machines have it on both systems): the rest must hold without it.
# usage: tools/help-agents.sh [path to fux]   (default: target/release/fux)
set -eu
fux=$(cd "$(dirname "${1:-target/release/fux}")" && pwd)/fux
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" && ln -s "$fux" "$work/bin/fux"
"$fux" --help | grep -q 'fux start'
perl=; ! command -v perl > /dev/null 2>&1 || perl=1
# (The section runs from its heading to the next line that is all capitals.)
section='/^IF YOU HAVE PERL/ { skip = 1 } /^[A-Z][A-Z ,]*$/ { skip = 0 }'
"$fux" --help | awk -v perl="$perl" "$section"' !(skip && !perl)' | sed -n 's/^    \([^ ]\)/\1/p' > "$work/page.sh"
# Nothing outside that section names perl or python.
if "$fux" --help | awk "$section"' !skip' | grep -n -i -e perl -e python; then
  echo "help-agents: perl or python outside the optional section" >&2; exit 1
fi
[ "$(wc -l < "$work/page.sh")" -ge 40 ]
mkdir "$work/home"
(cd "$work" && PATH="$work/bin:$PATH" XDG_RUNTIME_DIR="$work" XDG_CONFIG_HOME="$work/config" HOME="$work/home" EDITOR=true sh -eux page.sh) > "$work/out" 2> "$work/trace" || {
  cat "$work/out" >&2; tail -n 5 "$work/trace" >&2; echo "help-agents: a command of the page failed" >&2; exit 1; }
# What the page says its commands print, they print; and what it started, it removed.
for said in '^alt$' '^main$' "the main screen's first row is line [0-9]* of the pane's text" '^exit code 3$' '^state exited 3$' '^agent-demo/ls/ exited 3 : sh -c ls /; exit 3$' '^<<t1:[1-9][0-9]*>>$' ' end 1$' '^/$' '^state killed 15$' '^state exited 2$' \
  'crash is dead, though its state says running' '^the write to the dead pane came back$' \
  'something of pane stubborn still runs' '^nothing of it is left$' 'col(row(ls w1) w2)' '│' '^command sh -c pwd; exec sleep 600$' \
  '^[0-9]* put main/screen$'; do
  grep -q -- "$said" "$work/out" || { cat "$work/out" >&2; echo "help-agents: no '$said'" >&2; exit 1; }
done
if [ -n "$perl" ]; then
  grep -q '^refused at once$' "$work/out" && grep -q '^crash holds no lock$' "$work/out" || { cat "$work/out" >&2; echo "help-agents: the perl forms" >&2; exit 1; }
fi
[ -d "$work/fux" ] && [ ! -e "$work/fux/agent-demo" ] && [ ! -e "$work/fux/agent-view" ]
# (And nothing in the session but what the page says a viewer leaves; its keys are where it says.)
[ -z "$(ls -A "$work/fux" | grep -v -x -e keyboard -e colours)" ] && [ -x "$work/config/fux/keys/C-b/v" ]
echo "help-agents: ok ($(wc -l < "$work/page.sh" | tr -d ' ') lines run${perl:+, the perl section among them})"
