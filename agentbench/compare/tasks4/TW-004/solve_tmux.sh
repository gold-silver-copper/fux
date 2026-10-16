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
"${T[@]}" resize-window -t lab:0 -x 90 -y 26
start "$P" console.py
for i in {1..8}; do
 waitline "$P" "^CATALOG page=$i" >/dev/null
 "${T[@]}" send-keys -t "$P" Down
 "${T[@]}" send-keys -t "$P" Enter
 line=$(waitline "$P" "^READY form page=$i value=")
 value=${line##*value=}; send "$P" "$value"
 if [[ $i == 4 ]]; then waitline "$P" '^VALIDATION SERVICE UNAVAILABLE' >/dev/null; "${T[@]}" send-keys -t "$P" F3; fi
 waitline "$P" '^READY save button:' >/dev/null
 # Emit a semantic SGR left-button press at column 8, row 12.
 "${T[@]}" send-keys -t "$P" -l -- $'\e[<0;8;12M'
 if [[ $i == 5 ]]; then
  waitline "$P" '^Resize requested:' >/dev/null
  "${T[@]}" resize-window -t lab:0 -x 110 -y 34
 fi
done
c=$(result "$P" catalog); n=$(result "$P" pages)
printf 'catalog=%s\npages=%s\n' "$c" "$n" > "$W/answer.txt"

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
