#!/usr/bin/env bash
set -euo pipefail
WORK=$1
FIXTURES=$2
trap 'tmux -L ab kill-server >/dev/null 2>&1 || true' EXIT

tmux -L ab new-session -d -s ref -x 180 -y 55 'bash --noprofile --norc -i'
pane=ref:0.0
screen() { tmux -L ab capture-pane -p -t "$pane" -S -20000; }
wait_for() {
    local pattern=$1 i
    for ((i=0; i<500; i++)); do
        VIEW=$(screen)
        if printf '%s\n' "$VIEW" | grep -Eq -- "$pattern"; then return 0; fi
        sleep 0.1
    done
    printf 'Timed out waiting for %s\n%s\n' "$pattern" "$VIEW" >&2
    return 1
}
text() { tmux -L ab send-keys -t "$pane" -l -- "$1"; }
keys() { tmux -L ab send-keys -t "$pane" "$@"; }
submit() { text "$1"; keys Enter; }
# Only values visible in captures (or the task's desired configuration) are used.

printf -v command 'python3 -I %q %q' "$FIXTURES/revision_console.py" "$WORK"
submit "$command"
wait_for '^DRAFT TO TYPE: '
draft=$(printf '%s\n' "$VIEW" | sed -n 's/^DRAFT TO TYPE: //p' | tail -n 1)
text "$draft"
wait_for '^REVISION READY revision='
wait_for '^SUBMIT EXACTLY: '
replacement=$(printf '%s\n' "$VIEW" | sed -n 's/^SUBMIT EXACTLY: //p' | tail -n 1)
keys C-u
submit "$replacement"
wait_for '^PROGRAM RESULT revision=[0-9A-F]+ receipt=[0-9A-F]+$'
result=$(printf '%s\n' "$VIEW" | sed -n 's/^PROGRAM RESULT revision=\([0-9A-F]*\) receipt=\([0-9A-F]*\)$/revision=\1\nreceipt=\2/p' | tail -n 2)
printf '%s\n' "$result" > "$WORK/answer.txt"
