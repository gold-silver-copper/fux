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

printf -v command 'cd %q' "$WORK"
submit "$command"
submit 'export TASK_SHELL_FLAGS="$-"'
submit "PS1='forge status=\$? > '"
printf -v command 'python3 -I %q build %q' "$FIXTURES/shell_release.py" "$WORK"
submit "$command"
wait_for '^INSPECT READY$'
wait_for '^SUBMIT EXACTLY: inspect '
inspection=$(printf '%s\n' "$VIEW" | sed -n 's/^SUBMIT EXACTLY: inspect /inspect /p' | tail -n 1)
submit "$inspection"
wait_for '^BUILD FINISHED\. Read the exit status from your shell\.$'
wait_for '^forge status=[1-9][0-9]* > ?$'
# Bash expands $? on the right-hand side before assigning the saved variable.
# This is the first command after the build, and then attestation uses the value.
printf -v command 'build_status=$?; python3 -I %q attest %q "$build_status"' "$FIXTURES/shell_release.py" "$WORK"
submit "$command"
wait_for '^ATTEST READY; '
wait_for '^SUBMIT EXACTLY: seal '
seal_request=$(printf '%s\n' "$VIEW" | sed -n 's/^SUBMIT EXACTLY: seal /seal /p' | tail -n 1)
submit "$seal_request"
wait_for '^ATTESTED status=[0-9]+ seal=[0-9A-F]+$'
result=$(printf '%s\n' "$VIEW" | sed -n 's/^ATTESTED status=\([0-9]*\) seal=\([0-9A-F]*\)$/status=\1\nseal=\2/p' | tail -n 2)
printf '%s\n' "$result" > "$WORK/answer.txt"
