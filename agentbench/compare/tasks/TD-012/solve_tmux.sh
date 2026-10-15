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

launch "$pane" release_watch.py
wait_text "$pane" 'Follow>'
request=$(field 's/.*Enter exactly: (watch .*)/\1/p')
send "$pane" "$request"
wait_text "$pane" 'WATCH OPEN ticket='
ticket=$(field 's/.*WATCH OPEN ticket=([A-F0-9]+).*/\1/p')
# Match only records after WATCH OPEN, excluding the historical REVIEW.
wait_text "$pane" 'WINDOW OPEN ticket='
# The earlier Settle> is a decoy. Require the one after WINDOW OPEN;
# capture-pane removes a prompt's trailing space.
wait_text "$pane" $'WINDOW OPEN ticket='"$ticket"$'\nSubmit: settle <first-kind> <first-token>\nSettle>'
event=$(printf '%s\n' "$snapshot" | awk '/WATCH OPEN ticket=/{current=1;next} current && /^(REVIEW|COMPLETE|FAULT) ticket=/{print;exit}')
kind=$(printf '%s\n' "$event" | cut -d' ' -f1)
first=${event##*token=}
send "$pane" "settle $kind $first"
wait_text "$pane" 'SETTLED receipt='
wait_dead "$pane"
receipt=$(field 's/.*SETTLED receipt=([A-F0-9]+).*/\1/p')
printf 'ticket=%s\nfirst=%s:%s\nreceipt=%s\n' "$ticket" "$kind" "$first" "$receipt" > "$work/answer.txt"
