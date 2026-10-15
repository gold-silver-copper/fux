#!/usr/bin/env bash
set -euo pipefail
work=$1
fixtures=$2
# The validator owns the private server. Avoid racing its shutdown by
# managing only our session, rather than killing and recreating the server.
trap 'tmux -L ab kill-session -t task 2>/dev/null || true' EXIT
tmux -L ab -f /dev/null new-session -d -s task -x 100 -y 30
tmux -L ab set-option -g history-limit 70000
tmux -L ab set-option -g remain-on-exit on
pane=task:0.0
# Read only terminal output, never fixture records.
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

launch "$pane" audit_console.py
# capture-pane trims spaces at the end of the rendered prompt.
wait_text "$pane" 'Start>'
start=$(field 's/.*enter exactly: begin ([A-F0-9]+).*/\1/p')
# Preserve the terminal's emitted stream before starting the audit. Polling
# rendered screens can miss the first event when the host is busy, even with
# replay. pipe-pane leaves the fixture's stdin and stdout attached to its tty.
terminal_log="$work/audit-terminal.log"
printf -v capture_cmd 'exec cat > %q' "$terminal_log"
tmux -L ab pipe-pane -O -t "$pane" "$capture_cmd"
transcript() {
    if [[ -f "$terminal_log" ]]; then
        LC_ALL=C tr '\r' '\n' < "$terminal_log" |
            sed -E 's/\x1b\[[0-?]*[ -/]*[@-~]//g'
    fi
}
send "$pane" "begin $start"
wait_text "$pane" 'Decision>'
# Wait for the stream consumer as well as the displayed prompt; pipe output
# may still be draining after the rendered screen has reached Decision>.
limit=$((SECONDS+100))
while :; do
    emitted=$(transcript)
    [[ "$emitted" != *"SEQUENCE CLOSED $start"* ]] || break
    if (( SECONDS >= limit )); then
        printf 'Terminal stream did not reach sequence closure\n' >&2
        exit 1
    fi
    sleep 0.05
done
event=$(printf '%s\n' "$emitted" | awk '/^CURRENT AUDIT .*opening/{current=1;next} current && /^EVENT (ACCEPT|DENY|MANUAL) token=/{print;exit}')
[[ -n "$event" ]] || { printf 'Missing first current event\n' >&2; exit 1; }
kind=$(printf '%s\n' "$event" | cut -d' ' -f2)
first=${event##*token=}
first=${first%% *}
send "$pane" "choose $kind $first"
wait_text "$pane" 'AUDIT RECEIPT '
wait_dead "$pane"
[[ $(tmux -L ab display-message -p -t "$pane" '#{pane_dead_status}') == 0 ]]
receipt=$(field 's/.*AUDIT RECEIPT ([A-F0-9]+).*/\1/p')
printf 'first=%s:%s\nreceipt=%s\n' "$kind" "$first" "$receipt" > "$work/answer.txt"
