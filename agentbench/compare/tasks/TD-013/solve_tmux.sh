#!/usr/bin/env bash
set -euo pipefail
work=$1
fixtures=$2
tmux -L ab kill-server 2>/dev/null || true
trap 'tmux -L ab kill-server 2>/dev/null || true' EXIT
tmux -L ab -f /dev/null new-session -d -s task -x 100 -y 30
tmux -L ab set-option -g history-limit 70000
tmux -L ab set-option -g remain-on-exit on
pane=task:0.0
# Poll only the terminal's rendered output, never fixture records.
screen() { tmux -L ab capture-pane -p -t "$1" -S -70000; }
wait_text() {
    local target=$1 needle=$2 limit=$((SECONDS+100))
    while :; do
        snapshot=$(screen "$target")
        if [[ "$snapshot" == *"$needle"* ]]; then return; fi
        if (( SECONDS >= limit )); then printf 'Missing screen marker: %s\n' "$needle" >&2; exit 1; fi
        sleep 0.05
    done
}
send() { tmux -L ab send-keys -t "$1" -l -- "$2"; tmux -L ab send-keys -t "$1" Enter; }
launch() {
    local target=$1 script=$2
    shift 2
    printf -v launch_cmd 'python3 -I %q %q' "$fixtures/$script" "$work"
    local arg
    for arg in "$@"; do printf -v launch_cmd '%s %q' "$launch_cmd" "$arg"; done
    tmux -L ab respawn-pane -k -t "$target" "$launch_cmd"
}
field() { printf '%s\n' "$snapshot" | sed -nE "$1" | tail -n 1; }
wait_dead() {
    local target=$1 limit=$((SECONDS+100))
    until [[ $(tmux -L ab display-message -p -t "$target" '#{pane_dead}') == 1 ]]; do
        (( SECONDS < limit )) || exit 1
        sleep 0.05
    done
}

# A fresh window inherits the large history limit before its first output.
tmux -L ab new-window -d -t task -n archive
pane=task:archive.0
tmux -L ab resize-window -t task:archive -x 72 -y 18
launch "$pane" archive_reader.py
# capture-pane strips trailing blanks, including the space after a prompt.
wait_text "$pane" 'Stream>'
archive=$(field 's/^ARCHIVE ([A-F0-9]+).*/\1/p')
request=$(field 's/.*Enter exactly: stream ([A-F0-9]+).*/\1/p')
send "$pane" "stream $request"
wait_text "$pane" 'Resize>'
resume=$(field 's/.*enter exactly: continue ([A-F0-9]+).*/\1/p')
tmux -L ab resize-window -t task:archive -x 113 -y 31
send "$pane" "continue $resume"
wait_text "$pane" 'Seal>'
# Join soft-wrapped rows; PAYLOAD END terminates the logical payload.
snapshot=$(tmux -L ab capture-pane -p -J -t "$pane" -S -70000)
tail=$(printf '%s\n' "$snapshot" | sed -nE 's/^PAYLOAD .* tail=([A-F0-9]{16}) *$/\1/p' | tail -n 1)
send "$pane" "seal $archive $tail"
wait_text "$pane" 'SEALED receipt='
wait_dead "$pane"
snapshot=$(screen "$pane")
receipt=$(field 's/.*SEALED receipt=([A-F0-9]+).*/\1/p')
printf 'archive=%s\ntail=%s\nreceipt=%s\n' "$archive" "$tail" "$receipt" > "$work/answer.txt"
