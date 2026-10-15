#!/usr/bin/env bash
set -euo pipefail
work=$1
fixtures=$2
# Validation supplies a fresh private server. Reuse it: killing it immediately
# before new-session races with the old server's shutdown.
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
        if (( SECONDS >= limit )); then
            printf 'Missing screen marker: %s\n%s\n' "$needle" "$snapshot" >&2
            exit 1
        fi
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

tmux -L ab respawn-pane -k -t "$pane" "env HISTFILE= PS1='LEDGER-SHELL> ' bash --noprofile --norc -i"
# capture-pane trims the prompt's trailing space on an otherwise empty line.
wait_text "$pane" 'LEDGER-SHELL>'
printf -v cmd 'python3 -I %q %q' "$fixtures/ledger_job.py" "$work"
send "$pane" "$cmd"
wait_text "$pane" 'JOB DRAINED: returning control'
# The printed JOB DRAINED precedes exit; the real shell prompt must follow it.
limit=$((SECONDS+90))
while :; do
    snapshot=$(screen "$pane")
    if printf '%s\n' "$snapshot" | awk '/JOB DRAINED: returning control/{drained=1;next} drained && /^LEDGER-SHELL> *$/{found=1} END{exit !found}'; then break; fi
    (( SECONDS < limit )) || exit 1
    sleep 0.05
done
printf -v cmd 'python3 -I %q %q "$?" "$-"' "$fixtures/shell_receipt.py" "$work"
send "$pane" "$cmd"
wait_text "$pane" 'STATUS RECEIPT status='
status=$(field 's/.*STATUS RECEIPT status=([0-9]+) receipt=.*/\1/p')
receipt=$(field 's/.*STATUS RECEIPT status=[0-9]+ receipt=([A-F0-9]+).*/\1/p')
limit=$((SECONDS+90))
while :; do
    snapshot=$(screen "$pane")
    if printf '%s\n' "$snapshot" | awk '/STATUS RECEIPT status=/{reported=1;next} reported && /^LEDGER-SHELL> *$/{found=1} END{exit !found}'; then break; fi
    (( SECONDS < limit )) || exit 1
    sleep 0.05
done
send "$pane" exit
wait_dead "$pane"
printf 'status=%s\nreceipt=%s\n' "$status" "$receipt" > "$work/answer.txt"
