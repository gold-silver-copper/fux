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

R=lab:0.0
I=$("${T[@]}" split-window -h -t "$R" -P -F '#{pane_id}')
X=$("${T[@]}" split-window -v -t "$I" -P -F '#{pane_id}')
"${T[@]}" select-pane -t "$R" -T runner-ledger
"${T[@]}" select-pane -t "$I" -T inspector-ledger
"${T[@]}" select-pane -t "$X" -T repairer-ledger
start "$R" console.py runner
start "$I" console.py inspector
start "$X" console.py repairer
for i in {1..8}; do challenge "$R" runner "$i" run; done
waitline "$R" '^READY leave:' >/dev/null
send "$R" continue-unwatched
# No viewers are attached to this private server. Drop all activity until the checkpoint.
sleep 12
# Rediscover the runner by its title, as its checkpoint changes that title.
for ((lookup=0;lookup<300;lookup++)); do
 R=$("${T[@]}" list-panes -a -F '#{pane_id} #{pane_title}' | awk '$2=="runner-checkpoint-ledger" {print $1; exit}')
 [[ -n $R ]] && break
 sleep .1
done
[[ -n $R ]]
line=$(waitline "$R" '^UNATTENDED CHECKPOINT ticket='); ticket=${line##*ticket=}
waitline "$I" '^READY inspect:' >/dev/null; send "$I" "inspect $ticket"
for i in {1..4}; do challenge "$I" inspector "$i" inspect; done
d=$(result "$I" diagnosis)
waitline "$X" '^READY repair:' >/dev/null; send "$X" "repair $d"
for i in {1..4}; do challenge "$X" repairer "$i" repair; done
r=$(result "$X" repair)
for i in {9..14}; do challenge "$R" runner "$i" run; done
l=$(result "$R" ledger)
printf 'checkpoint=%s\ndiagnosis=%s\nrepair=%s\nledger=%s\n' "$ticket" "$d" "$r" "$l" > "$W/answer.txt"

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
