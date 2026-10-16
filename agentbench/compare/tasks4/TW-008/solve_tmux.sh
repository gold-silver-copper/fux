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
# The real program is started in a fresh pane after setting the deep history limit.
Q=$("${T[@]}" split-window -h -t "$P" -P -F '#{pane_id}')
"${T[@]}" resize-window -t lab:0 -x 170 -y 50
"${T[@]}" kill-pane -t "$P"
start "$Q" console.py
waitline "$Q" '^READY retrieval vault:' >/dev/null
vault=$("${T[@]}" capture-pane -p -J -S - -t "$Q" | python3 -c 'import re,sys; s=sys.stdin.read(); print(re.search(r"VAULT-RECORD .*? key=([A-F0-9]+) END-VAULT",s).group(1))')
count=$(cap "$Q" | python3 -c 'import re,sys; print(re.search(r"AUDIT-COUNT=(\d+)",sys.stdin.read()).group(1))')
send "$Q" "vault $vault"
waitline "$Q" '^READY retrieval count:' >/dev/null
send "$Q" "count $count"
for i in {1..20}; do
 slot=$(( ((i-1)*7)%20+1 ))
 waitline "$Q" "^READY bookmark slot=$slot;" >/dev/null
 key=$(cap "$Q" | python3 -c 'import re,sys; slot=sys.argv[1]; print(re.search(r"^BOOKMARK slot="+slot+r" key=([A-F0-9]+) END-BOOKMARK$",sys.stdin.read(),re.M).group(1))' "$slot")
 send "$Q" "bookmark $key"
done
index=$(result "$Q" index)
printf 'vault=%s\ncount=%s\nindex=%s\n' "$vault" "$count" "$index" > "$W/answer.txt"

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
