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

N=lab:0.0
E=$("${T[@]}" split-window -h -t "$N" -P -F '#{pane_id}')
V=$("${T[@]}" split-window -v -t "$E" -P -F '#{pane_id}')
"${T[@]}" select-layout -t lab:0 even-horizontal
"${T[@]}" resize-window -t lab:0 -x 150 -y 44
start "$N" console.py north
start "$E" console.py east
start "$V" console.py west
for i in {1..8}; do
 if [[ $i == 5 ]]; then "${T[@]}" resize-window -t lab:0 -x 210 -y 48; "${T[@]}" select-layout -t lab:0 even-horizontal; fi
 for pair in "$N north" "$E east" "$V west"; do read -r p r <<< "$pair"; challenge "$p" "$r" "$i" seal; done
 for row in "$N north east $E" "$E east west $V" "$V west north $N"; do
  read -r p r other q <<< "$row"
  line=$(waitline "$q" "^CERT epoch=$i owner=$other value=")
  cert=${line##*value=}
  waitline "$p" "^READY exchange epoch=$i;" >/dev/null
  send "$p" "import $other $cert"
 done
done
: > "$W/answer.txt"
for pair in "$N north" "$E east" "$V west"; do
 read -r p r <<< "$pair"
 line=$(waitline "$p" '^READY shutdown:'); send "$p" "${line#*exactly: }"
 c=$(result "$p" certificate)
 printf '%s=%s\n' "$r" "$c" >> "$W/answer.txt"
done

# Wait for completed programs (and the interactive shell, where used) to exit.
for ((tries=0;tries<200;tries++)); do
 live=$("${T[@]}" list-panes -s -t lab -F '#{pane_dead}' | python3 -c 'import sys; print(sum(x.strip()=="0" for x in sys.stdin))')
 [[ $live == 0 ]] && break
 sleep .05
done
[[ $live == 0 ]]
