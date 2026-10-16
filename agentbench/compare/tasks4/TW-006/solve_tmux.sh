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

P=lab:0.0
J=$("${T[@]}" split-window -h -t "$P" -P -F '#{pane_id}')
start "$J" console.py journal
# Replace the foreground pane with a known interactive shell; $- is expanded there.
"${T[@]}" respawn-pane -k -t "$P" 'exec bash --noprofile --norc -i'
send "$P" "PS1='LAB-SHELL> '"
for i in {1..6}; do challenge "$J" journal "$i" checkpoint; done
waitline "$J" '^PARKED journal:' >/dev/null
printf -v cmd 'python3 -I %q %q command "$-"; migration_status=$?; printf "SHELL-STATUS=%%s\\n" "$migration_status"' "$F/console.py" "$W"
send "$P" "$cmd"
for i in {1..10}; do challenge "$P" command "$i" apply; done
line=$(waitline "$P" '^SHELL-STATUS='); s=${line#*=}
m=$(result "$P" migration)
printf -v cmd 'python3 -I %q %q audit' "$F/console.py" "$W"
send "$P" "$cmd"
waitline "$P" '^READY status audit:' >/dev/null
send "$P" "status $s"
for i in {1..6}; do challenge "$P" audit "$i" audit; done
a=$(result "$P" audit)
line=$(waitline "$J" '^READY shutdown:'); send "$J" "${line#*exactly: }"
j=$(result "$J" journal)
printf 'status=%s\nmigration=%s\naudit=%s\njournal=%s\n' "$s" "$m" "$a" "$j" > "$W/answer.txt"
send "$P" exit

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
