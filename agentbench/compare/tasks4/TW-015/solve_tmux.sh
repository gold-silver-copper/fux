#!/usr/bin/env bash
set -euo pipefail
W=$1
F=$2
mkdir -p "$W"
cd "$W"
t() { tmux -L ab "$@"; }
t kill-server 2>/dev/null || true
t new-session -d -s bootstrap -x 260 -y 70
t set-option -g history-limit 60000
t new-session -d -s run -x 260 -y 70
t kill-session -t bootstrap
P=$(t display-message -p -t run:0.0 '#{pane_id}')
t set-option -g remain-on-exit on
snap() { t capture-pane -p -J -t "$1" -S -60000; }
waitfor() {
  local p=$1 pat=$2 out
  for ((z=0; z<1800; z++)); do
    out=$(snap "$p")
    if [[ "$out" == *"$pat"* ]]; then return; fi
    if [[ "$out" == *"FAILED "* || "$out" == *"Traceback (most recent call last)"* ]]; then printf '%s\n' "$out" >&2; exit 1; fi
    sleep .05
  done
  printf 'Missing: %s\n' "$pat" >&2
  snap "$p" >&2
  exit 1
}
value() { snap "$1" | sed -nE "$2" | tail -n 1; }
send() { t send-keys -t "$1" -l -- "$2"; t send-keys -t "$1" Enter; }
declare -A EXIT_TAGS
LAUNCH_SERIAL=0
launch() {
  local p=$1 name=$2
  shift 2
  LAUNCH_SERIAL=$((LAUNCH_SERIAL+1))
  EXIT_TAGS[$p]="FXRETURN_$LAUNCH_SERIAL"
  send "$p" "python3 -I '$F/$name.py' '$W' $*; printf '\\nFXRETURN_$LAUNCH_SERIAL=%s\\n' \"\$?\""
}
wait_returned() {
  local p=$1 tag=${EXIT_TAGS[$1]} found
  for ((j=0;j<1800;j++)); do
    found=$(t capture-pane -p -J -t "$p" -S -100 | sed -nE "s/^$tag=([0-9]+).*$/\\1/p")
    [[ -n "$found" ]] && return
    sleep .05
  done
  printf 'Program has not returned to its shell: %s\n' "$p" >&2
  exit 1
}
finish_answer() { snap "$P" | sed -n 's/^RESULT //p' | tr -d '\r' > "$W/answer.txt"; }

t resize-window -t run:0 -x 100 -y 35
launch "$P" board
for ((n=1;n<=8;n++)); do
 nn=$(printf '%02d' "$n")
 if ((n==5)); then waitfor "$P" 'RESIZE BOARD>'; t resize-window -t run:0 -x 130 -y 42; t send-keys -t "$P" Enter; fi
 waitfor "$P" "BOARD $nn>"
 t send-keys -t "$P" -l $'\033[<0;3;5M'
 waitfor "$P" "DETAIL $nn:"
 c=$(value "$P" 's/.*NOTICE_CODE=([0-9a-f]+).*$/\1/p')
 if ((n==4)); then
   waitfor "$P" 'RECOVERY: notice locked.'
   u=$(value "$P" 's/.*Enter unlock ([0-9a-f]+).*$/\1/p')
   send "$P" "unlock $u"
   waitfor "$P" 'DETAIL 04 unlocked:'
 fi
 send "$P" "acknowledge $c"
 waitfor "$P" "REVIEW $nn>"
 t send-keys -t "$P" F2
 waitfor "$P" "SAVE $nn>"
 t send-keys -t "$P" Enter
done
waitfor "$P" COMPLETE
finish_answer

for returned_pane in "${!EXIT_TAGS[@]}"; do
  wait_returned "$returned_pane"
done
t kill-server
