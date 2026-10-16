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
"${T[@]}" resize-window -t lab:0 -x 82 -y 24
start "$P" console.py
for i in {1..7}; do
 waitline "$P" "^PATCHDESK revision=$i" >/dev/null
 "${T[@]}" send-keys -t "$P" NPage
 "${T[@]}" send-keys -t "$P" NPage
 "${T[@]}" send-keys -t "$P" F4
 waitline "$P" "^READY editor revision=$i" >/dev/null
 # Join physical wraps, then read only the reference seal from the rendered terminal.
 seal=$("${T[@]}" capture-pane -p -J -t "$P" | python3 -c 'import re,sys; print(re.search(r"REFERENCE .*? seal=([A-F0-9]+) end-reference",sys.stdin.read()).group(1))')
 printf 'BEGIN %s\nseal %s\nEND\n' "$i" "$seal" | "${T[@]}" load-buffer -
 "${T[@]}" paste-buffer -t "$P"
 waitline "$P" "^READY review revision=$i:" >/dev/null
 "${T[@]}" send-keys -t "$P" F2
 waitline "$P" '^COMMIT MODAL:' >/dev/null
 "${T[@]}" send-keys -t "$P" Right
 "${T[@]}" send-keys -t "$P" Enter
 if [[ $i == 3 ]]; then waitline "$P" '^Widen diff viewer' >/dev/null; "${T[@]}" resize-window -t lab:0 -x 114 -y 32; fi
done
p=$(result "$P" patchset); c=$(result "$P" commits)
printf 'patchset=%s\ncommits=%s\n' "$p" "$c" > "$W/answer.txt"

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
