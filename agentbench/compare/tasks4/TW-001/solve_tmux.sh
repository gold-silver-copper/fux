#!/usr/bin/env bash
set -euo pipefail
W=$1
F=$2
export W F
T=(tmux -L ab)
"${T[@]}" new-session -d -s lab -x 150 -y 48
"${T[@]}" set-option -g history-limit 100000
"${T[@]}" set-option -g remain-on-exit on
# All observations below come from the terminal screen or scrollback.
cap() { "${T[@]}" capture-pane -p -S - -t "$1"; }
waitline() {
 local pane=$1 pattern=$2 found
 for ((tries=0;tries<1600;tries++)); do
  found=$(cap "$pane" | python3 -c 'import re,sys; p=re.compile(sys.argv[1]); a=[x.rstrip("\r") for x in sys.stdin.read().splitlines() if p.search(x)]; print(a[-1] if a else "")' "$pattern")
  if [[ -n $found ]]; then printf '%s\n' "$found"; return; fi
  sleep .05
 done
 echo "Timed out awaiting $pattern in $pane" >&2; cap "$pane" >&2; return 1
}
send() { "${T[@]}" send-keys -t "$1" -l -- "$2"; "${T[@]}" send-keys -t "$1" Enter; }
start() {
 local pane=$1 program=$2 args=${3:-}
 local command
 printf -v command 'exec python3 -I %q %q %s' "$F/$program" "$W" "$args"
 "${T[@]}" send-keys -t "$pane" -l -- "$command"
 "${T[@]}" send-keys -t "$pane" Enter
}
challenge() {
 local pane=$1 role=$2 stage=$3 verb=$4 line token
 line=$(waitline "$pane" "^READY role=$role stage=$stage token=")
 token=${line##*token=}
 send "$pane" "$verb $token"
}
result() { local line; line=$(waitline "$1" "^RESULT $2="); printf '%s' "${line#*=}"; }

B=lab:0.0
S=$("${T[@]}" split-window -h -t "$B" -P -F '#{pane_id}')
C=$("${T[@]}" split-window -v -t "$S" -P -F '#{pane_id}')
"${T[@]}" resize-window -t lab:0 -x 180 -y 55
"${T[@]}" select-layout -t lab:0 tiled
start "$B" console.py build
start "$S" console.py server
start "$C" console.py canary
for i in {1..8}; do challenge "$B" build "$i" approve; done
line=$(waitline "$B" '^Compiler crashed after durable checkpoint')
receipt=${line##* }
waitline "$B" '^Compiler crashed' >/dev/null
for ((exit_poll=0;exit_poll<200;exit_poll++)); do
  dead=$("${T[@]}" display-message -p -t "$B" '#{pane_dead}')
  [[ $dead == 1 ]] && break
  sleep .05
 done
 [[ $dead == 1 ]]
printf -v command 'python3 -I %q %q rebuild %q' "$F/console.py" "$W" "$receipt"
"${T[@]}" respawn-pane -k -t "$B" "$command"
for i in {9..12}; do challenge "$B" rebuild "$i" approve; done
a=$(result "$B" artifact)
waitline "$S" '^READY artifact-input' >/dev/null
send "$S" "load $a"
for i in {1..6}; do challenge "$S" server "$i" deploy; done
e=$(result "$S" endpoint)
waitline "$C" '^READY endpoint-input' >/dev/null
send "$C" "target $e"
for i in {1..6}; do challenge "$C" canary "$i" verify; done
r=$(result "$C" release)
printf 'artifact=%s\nendpoint=%s\nrelease=%s\n' "$a" "$e" "$r" > "$W/answer.txt"

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
