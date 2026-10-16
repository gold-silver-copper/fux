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

declare -a panes ledger results
panes[0]=$P
for ((i=1;i<8;i++)); do
 panes[$i]=$(t split-window -v -t "$P" -P -F '#{pane_id}')
 t select-layout -t run:0 tiled
done
for ((i=0;i<8;i++)); do
 launch "${panes[$i]}" worker "$i"
done
t rename-session -t run sweep
# Visit an independent shell while all workers remain alive.
t new-session -d -s notebook
E=$(t display-message -p -t notebook:0.0 '#{pane_id}')
launch "$E" visit
waitfor "$E" 'Independent visit:'
send "$E" 'visit completed'
waitfor "$E" COMPLETE
wait_returned "$E"
unset "EXIT_TAGS[$E]"
t kill-session -t notebook
for ((i=0;i<8;i++)); do
 ii=$(printf '%02d' "$i")
 panes[$i]=$(t list-panes -a -F '#{pane_id} #{pane_title}' | awk -v title="TW-020-worker$ii" '$2==title {print $1; exit}')
 waitfor "${panes[$i]}" "ARCHIVE WORKER${ii}_KEY="
 ledger[$i]=$(value "${panes[$i]}" "s/^ARCHIVE WORKER${ii}_KEY=([0-9a-f]+).*$/\1/p")
done
for ((lap=1;lap<=3;lap++)); do
 for ((i=0;i<8;i++)); do
  ii=$(printf '%02d' "$i"); p=${panes[$i]}
  waitfor "$p" "WORKER $ii READY LAP $lap CODE"
  c=$(value "$p" "s/^WORKER $ii READY LAP $lap CODE ([0-9a-f]+).*$/\1/p")
  send "$p" "run $lap $c ${ledger[$i]}"
  waitfor "$p" "WORKER $ii DONE LAP $lap RESULT"
  if ((lap==3)); then results[$i]=$(value "$p" "s/^WORKER $ii DONE LAP 3 RESULT ([0-9a-f]+).*$/\1/p"); fi
 done
done
: > "$W/answer.txt"
for ((i=0;i<8;i++)); do
 p=${panes[$i]}; ii=$(printf '%02d' "$i")
 waitfor "$p" 'Worker remains running.'
 send "$p" "seal ${results[$i]}"
 waitfor "$p" COMPLETE
 value "$p" "s/^RESULT (worker=$ii .*)$/\1/p" | tr -d '\r' >> "$W/answer.txt"
done

for returned_pane in "${!EXIT_TAGS[@]}"; do
  wait_returned "$returned_pane"
done
t kill-server
