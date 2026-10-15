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

printf -v command 'python3 -I %q %q' "$FIXTURES/line_dispatch.py" "$WORK"
submit "$command"
wait_for '^EDITOR READY$'
wait_for '^SUBMIT EXACTLY: ship '
target=$(printf '%s\n' "$VIEW" | sed -n 's/^SUBMIT EXACTLY: ship /ship /p' | tail -n 1)
# Exercise every advertised editing operation on the existing line.
keys Home
text $'\033[3~'
text s
keys End BSpace C-w C-w
text "${target#ship }"
keys Left Right Home End Enter
wait_for '^SUBMIT EXACTLY: confirm receipt='
confirm=$(printf '%s\n' "$VIEW" | sed -n 's/^SUBMIT EXACTLY: confirm /confirm /p' | tail -n 1)
keys C-u
submit "$confirm"
wait_for '^COMMITTED route=[0-9A-F]+ receipt=[0-9A-F]+$'
result=$(printf '%s\n' "$VIEW" | sed -n 's/^COMMITTED route=\([0-9A-F]*\) receipt=\([0-9A-F]*\)$/route=\1\nreceipt=\2/p' | tail -n 2)
printf '%s\n' "$result" > "$WORK/answer.txt"
