#!/usr/bin/env bash
set -euo pipefail
WORK=$1
FIXTURES=$2
declare -A pane result status
tmux -L ab start-server
tmux -L ab set-option -g history-limit 100000
tmux -L ab set-option -g default-shell /bin/bash
tmux -L ab set-option -g remain-on-exit on
tmux -L ab new-session -d -s main -x 240 -y 90 -n control 'bash --noprofile --norc'
tmux -L ab set-option -g window-size manual
tmux -L ab resize-window -t main:control -x 240 -y 90

screen() { tmux -L ab capture-pane -p -J -t "$1"; }
history() { tmux -L ab capture-pane -p -J -S -100000 -t "$1"; }
await() {
    local p=$1 pattern=$2 view
    for ((try=0;try<1800;try++)); do
        view=$(screen "$p")
        if printf '%s\n' "$view" | grep -Eq "$pattern"; then return; fi
        if printf '%s\n' "$view" | grep -q 'RESTART REQUIRED'; then printf '%s\n' "$view" >&2; return 1; fi
        sleep .1
    done
    printf 'timeout waiting for %s\n' "$pattern" >&2
    screen "$p" >&2
    return 1
}
text() { tmux -L ab send-keys -t "$1" -l -- "$2"; }
enter() { tmux -L ab send-keys -t "$1" Enter; }
launch() {
    local role=$1 p
    p=$(tmux -L ab new-window -d -P -F '#{pane_id}' -t main -n "$role" 'bash --noprofile --norc')
    pane[$role]=$p
    tmux -L ab select-pane -t "$p" -T "$role"
    tmux -L ab resize-window -t "$p" -x 240 -y 90
    text "$p" "python3 -I '$FIXTURES/console.py' '$WORK' '$role'; fx_status=\$?; printf '\\nFIXTURE-EXIT $role status=%s\\n' \"\$fx_status\""
    enter "$p"
}
split_launch() {
    local role=$1 parent=$2 direction=$3 p
    p=$(tmux -L ab split-window -d "$direction" -t "${pane[$parent]}" -P -F '#{pane_id}' 'bash --noprofile --norc')
    pane[$role]=$p
    tmux -L ab select-pane -t "$p" -T "$role"
    text "$p" "python3 -I '$FIXTURES/console.py' '$WORK' '$role'; fx_status=\$?; printf '\\nFIXTURE-EXIT $role status=%s\\n' \"\$fx_status\""
    enter "$p"
}
step() {
    local role=$1 n=$2 verb=$3 source=${4:-} kind=${5:-normal} p=${pane[$1]} nn token display
    printf -v nn '%02d' "$n"
    if [[ $kind == tui ]]; then
        await "$p" "^TARGET $role step=$nn action=$verb"
        for ((d=0;d<1+n%2;d++)); do tmux -L ab send-keys -t "$p" Down; done
        tmux -L ab send-keys -t "$p" Enter
    fi
    if [[ $kind == quiet ]]; then
        await "$p" "^DRAIN $role step=$nn "
        local before after stable=0
        before=$(screen "$p")
        while ((stable<4)); do
            sleep 1
            after=$(screen "$p")
            if [[ $before == "$after" ]]; then stable=$((stable+1)); else stable=0; fi
            before=$after
        done
    else
        await "$p" "^INPUT $role step=$nn ready"
    fi
    if [[ -n $source ]]; then
        token=${result[$source]}
        if [[ $role == monitor ]]; then token="${status[$source]} $token"; fi
    else
        display=$(history "$p")
        token=$(printf '%s\n' "$display" | sed -nE "s/.*(CHALLENGE|ARCHIVE) $role step=$nn code=([0-9a-f]{12}).*/\2/p" | tail -1)
        [[ -n $token ]] || { printf 'missing token %s %s\n' "$role" "$nn" >&2; return 1; }
    fi
    text "$p" "$verb $token"
    enter "$p"
    await "$p" "^ACCEPTED $role step=$nn|^TARGET $role step=|^RESULT $role receipt="
}
collect() {
    local role=$1 p=${pane[$1]}
    await "$p" "^RESULT $role receipt="
    result[$role]=$(screen "$p" | sed -nE "s/^RESULT $role receipt=([0-9a-f]{12}).*/\1/p" | tail -1)
    [[ -n ${result[$role]} ]]
}
close_role() {
    local role=$1 p=${pane[$1]}
    text "$p" "close ${result[$role]}"
    enter "$p"
    await "$p" "^FIXTURE-EXIT $role status=0"
    # Exit the surrounding shell too, and observe that the pane process exited.
    text "$p" 'exit'
    enter "$p"
    for ((attempt=0;attempt<200;attempt++)); do
        [[ $(tmux -L ab display-message -p -t "$p" '#{pane_dead}') == 1 ]] && return
        sleep .1
    done
    return 1
}
attach_view() {
    # Hold the viewer's terminal open without sending EOF or typed input.
    python3 - "$WORK/$1.log" <<'PYCLIENT' &
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 90, 240, 0, 0))
def controlling_terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
env = dict(os.environ, TERM='xterm-256color')
client = subprocess.Popen(['tmux', '-L', 'ab', 'attach-session', '-t', 'shift-vault'],
                          stdin=slave, stdout=slave, stderr=slave,
                          preexec_fn=controlling_terminal, env=env)
os.close(slave)
try:
    with open(sys.argv[1], 'wb') as log:
        while client.poll() is None:
            if select.select([master], [], [], .1)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                log.write(data)
    raise SystemExit(client.wait())
finally:
    os.close(master)
PYCLIENT
    visit_pid=$!
    for ((a=0;a<100;a++)); do
        [[ -n $(tmux -L ab list-clients -t shift-vault -F '#{client_name}') ]] && return
        sleep .1
    done
    return 1
}
tmux -L ab new-session -d -s shift-vault -x 240 -y 90 -n retained-service 'bash --noprofile --norc'
pane[daemon]=$(tmux -L ab display-message -p -t shift-vault:retained-service '#{pane_id}')
tmux -L ab select-pane -t "${pane[daemon]}" -T retained-service
text "${pane[daemon]}" "python3 -I '$FIXTURES/console.py' '$WORK' daemon; fx_status=\$?; printf '\\nFIXTURE-EXIT daemon status=%s\\n' \"\$fx_status\""
enter "${pane[daemon]}"
launch ledger
launch inspector
step daemon 0 boot
step daemon 1 load
step daemon 2 subscribe
step daemon 3 checkpoint
step daemon 4 park
await "${pane[daemon]}" '^PARKED retained-service'
attach_view client-visit
tmux -L ab detach-client -s shift-vault
wait "$visit_pid" || true
step ledger 0 open
step ledger 1 index
step ledger 2 compare
step ledger 3 hash
step ledger 4 attest
step ledger 5 export
step ledger 6 commit
step ledger 7 seal
collect ledger
attach_view client-return
tmux -L ab select-window -t shift-vault:retained-service
tmux -L ab select-pane -t "${pane[daemon]}"
await "${pane[daemon]}" '^RESUME-READY epoch='
step daemon 5 resume
step daemon 6 flush
step daemon 7 seal
collect daemon
tmux -L ab detach-client -s shift-vault
wait "$visit_pid" || true
step inspector 0 attach "daemon"
step inspector 1 identity ""
step inspector 2 epoch ""
step inspector 3 compare "ledger"
step inspector 4 audit ""
step inspector 5 drain ""
step inspector 6 report ""
step inspector 7 seal ""
collect inspector
close_role inspector
close_role ledger
close_role daemon
: >"$WORK/answer.txt"
printf '%s\n' 'daemon='"${result[daemon]}" >>"$WORK/answer.txt"
printf '%s\n' 'ledger='"${result[ledger]}" >>"$WORK/answer.txt"
printf '%s\n' 'inspector='"${result[inspector]}" >>"$WORK/answer.txt"
