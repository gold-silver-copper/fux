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

# One window per worker preserves enough rows while all eight workers remain live.
PANES=()
PANES[0]=lab:0.0
for i in {1..7}; do PANES[$i]=$("${T[@]}" new-window -d -t lab -n "worker$i" -P -F '#{pane_id}'); done
for i in {0..7}; do "${T[@]}" select-pane -t "${PANES[$i]}" -T "worker$i"; start "${PANES[$i]}" console.py "$i"; done
for round in {1..3}; do
 for i in {0..7}; do
  p=${PANES[$i]}
  if [[ $round -gt 1 ]]; then
   prev=$(( (i+7)%8 )); q=${PANES[$prev]}
   line=$(waitline "$q" "^ROUND result round=$((round-1)) value="); v=${line##*value=}
   waitline "$p" "^READY dependency round=$round;" >/dev/null
   send "$p" "previous $v"
  fi
  challenge "$p" "worker$i" "$round" commit
 done
 if [[ $round == 1 ]]; then
  line=$(waitline "${PANES[3]}" '^CRASH checkpoint saved.'); receipt=${line##* }
  for ((exit_poll=0;exit_poll<200;exit_poll++)); do
  dead=$("${T[@]}" display-message -p -t "${PANES[3]}" '#{pane_dead}')
  [[ $dead == 1 ]] && break
  sleep .05
 done
 [[ $dead == 1 ]]
  printf -v command 'python3 -I %q %q 3 resume %q' "$F/console.py" "$W" "$receipt"
  "${T[@]}" respawn-pane -k -t "${PANES[3]}" "$command"
 fi
done
: > "$W/answer.txt"
for i in {0..7}; do r=$(result "${PANES[$i]}" result); printf 'worker%s=%s\n' "$i" "$r" >> "$W/answer.txt"; done

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
