#!/usr/bin/env bash
set -euo pipefail
work=$1
fixtures=$2
trap 'tmux -L ab kill-server 2>/dev/null || true' EXIT
# Reuse the validator's private server rather than racing its shutdown.
tmux -L ab -f /dev/null new-session -d -s task -x 100 -y 30
tmux -L ab set-option -g history-limit 70000
tmux -L ab set-option -g remain-on-exit on
pane=task:0.0
# Poll only the terminal's rendered output, never fixture records.
screen() { tmux -L ab capture-pane -p -J -t "$1" -S -70000; }
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

tmux -L ab split-window -h -t "$pane"
tmux -L ab split-window -v -t task:0.1
build=task:0.0
server=task:0.1
probe=task:0.2
launch "$build" builder.py
launch "$server" preview_server.py
launch "$probe" test_probe.py
# capture-pane may trim the trailing space of a prompt.
wait_text "$build" 'Build>'
request=$(field 's/.*Enter exactly: compile ([A-F0-9]+).*/\1/p')
send "$build" "compile $request"
wait_text "$build" 'Build ready>'
artifact=$(field 's/^ARTIFACT ([A-F0-9]+).*/\1/p')
wait_text "$server" 'Attach>'
send "$server" "attach $artifact"
wait_text "$server" 'Settle server>'
session=$(field 's/^LIVE session=([A-F0-9]+).*/\1/p')
wait_text "$probe" 'Probe>'
send "$probe" "probe $session"
# REPORT CLOSED precedes more output. Wait for the input prompt as well,
# so even a descheduled probe cannot receive quit while still printing.
wait_text "$probe" 'Probe done>'
[[ "$snapshot" == *'REPORT CLOSED'* ]]
event=$(printf '%s\n' "$snapshot" | awk '/PROBE OPEN session=/{current=1;next} current && /^(PASS|FAIL|BLOCKED) token=/{print;exit}')
kind=$(printf '%s\n' "$event" | cut -d' ' -f1)
first=$(printf '%s\n' "$event" | sed -nE 's/^(PASS|FAIL|BLOCKED) token=([A-F0-9]+).*/\2/p')
case $kind in PASS) action=commit;; FAIL) action=rollback;; BLOCKED) action=defer;; *) exit 1;; esac
send "$server" "$action $first"
wait_text "$server" 'Server settled>'
receipt=$(field 's/.*SETTLED receipt=([A-F0-9]+).*/\1/p')
send "$build" quit
send "$probe" quit
send "$server" quit
wait_text "$build" 'BUILDER BYE'
wait_text "$probe" 'PROBE BYE'
wait_text "$server" 'SERVER BYE'
wait_dead "$build"
wait_dead "$probe"
wait_dead "$server"
printf 'artifact=%s\nsession=%s\nfirst=%s:%s\nreceipt=%s\n' "$artifact" "$session" "$kind" "$first" "$receipt" > "$work/answer.txt"
