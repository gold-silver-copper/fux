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

A=$(t split-window -h -p 40 -t "$P" -P -F '#{pane_id}')
B=$(t split-window -v -p 50 -t "$A" -P -F '#{pane_id}')
launch "$P" pack
launch "$A" decode
launch "$B" sign
waitfor "$P" 'ARCHIVE END. Enter open'
declare -a packets
for ((i=1;i<=8;i++)); do
 ii=$(printf '%02d' "$i")
 packets[$i]=$(value "$P" "s/^PACKET ARCHIVE $ii ([0-9a-f]+).*$/\1/p")
done
send "$P" open
t rename-session -t run parcel-vault
t new-session -d -s independent
E=$(t display-message -p -t independent:0.0 '#{pane_id}')
launch "$E" visit
waitfor "$E" 'Independent visit:'
send "$E" 'visit completed'
waitfor "$E" COMPLETE
wait_returned "$E"
unset "EXIT_TAGS[$E]"
t kill-session -t independent
P=$(t list-panes -a -F '#{pane_id} #{pane_title}' | awk '$2=="TW-012-pack" {print $1; exit}')
for ((n=1;n<=8;n++)); do
 nn=$(printf '%02d' "$n")
 waitfor "$P" "ATTACH $nn>"
 item=$(value "$P" "s/^PACKAGE $nn ITEM ([0-9]+) CODE.*$/\1/p")
 i=$((10#$item))
 c=$(value "$P" "s/^PACKAGE $nn ITEM [0-9]+ CODE ([0-9a-f]+).*$/\1/p")
 waitfor "$A" "DECODER READY $nn:"
 send "$A" "decode $i ${packets[$i]}"
 waitfor "$A" "DECODED ITEM $item VALUE"
 d=$(value "$A" "s/^DECODED ITEM $item VALUE ([0-9a-f]+).*$/\1/p")
 waitfor "$B" "SIGNER READY $nn:"
 send "$B" "sign $i $d"
 waitfor "$B" "SIGNED ITEM $item VALUE"
 s=$(value "$B" "s/^SIGNED ITEM $item VALUE ([0-9a-f]+).*$/\1/p")
 send "$P" "attach $i $s $c"
 waitfor "$P" "ATTACHED $nn"
done
waitfor "$A" 'DECODER CLOSE:'
waitfor "$B" 'SIGNER CLOSE:'
send "$A" quit
send "$B" quit
waitfor "$A" COMPLETE
waitfor "$B" COMPLETE
waitfor "$P" 'Close decoder and signer'
send "$P" finish
waitfor "$P" COMPLETE
finish_answer

for returned_pane in "${!EXIT_TAGS[@]}"; do
  wait_returned "$returned_pane"
done
t kill-server
