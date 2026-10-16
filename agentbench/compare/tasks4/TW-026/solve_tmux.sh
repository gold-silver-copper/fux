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
launch monitor
pane[shell]=$(tmux -L ab new-window -d -P -F '#{pane_id}' -t main -n interactive 'bash --noprofile --norc -i')
tmux -L ab resize-window -t "${pane[shell]}" -x 240 -y 90
pane[j01]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j01' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j01 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j01 0 prepare
step j01 1 execute
collect j01
await "${pane[shell]}" "^SHELL-STATUS j01 status="
status[j01]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j01 status=([0-9]+).*/\1/p' | tail -1)
pane[j02]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j02' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j02 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j02 0 prepare
step j02 1 execute
collect j02
await "${pane[shell]}" "^SHELL-STATUS j02 status="
status[j02]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j02 status=([0-9]+).*/\1/p' | tail -1)
pane[j03]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j03' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j03 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j03 0 prepare
step j03 1 execute
collect j03
await "${pane[shell]}" "^SHELL-STATUS j03 status="
status[j03]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j03 status=([0-9]+).*/\1/p' | tail -1)
pane[j04]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j04' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j04 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j04 0 prepare
step j04 1 execute
collect j04
await "${pane[shell]}" "^SHELL-STATUS j04 status="
status[j04]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j04 status=([0-9]+).*/\1/p' | tail -1)
pane[j05]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j05' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j05 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j05 0 prepare
step j05 1 execute
collect j05
await "${pane[shell]}" "^SHELL-STATUS j05 status="
status[j05]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j05 status=([0-9]+).*/\1/p' | tail -1)
pane[j06]=${pane[shell]}
text "${pane[shell]}" "python3 -I '$FIXTURES/console.py' '$WORK' 'j06' --shell-flags \"\$-\"; fx_status=\$?; printf '\\nSHELL-STATUS j06 status=%s\\n' \"\$fx_status\""
enter "${pane[shell]}"
step j06 0 prepare
step j06 1 execute
collect j06
await "${pane[shell]}" "^SHELL-STATUS j06 status="
status[j06]=$(screen "${pane[shell]}" | sed -nE 's/^SHELL-STATUS j06 status=([0-9]+).*/\1/p' | tail -1)
step monitor 0 bind ""
step monitor 1 report-j01 "j01"
step monitor 2 report-j02 "j02"
step monitor 3 report-j03 "j03"
step monitor 4 report-j04 "j04"
step monitor 5 report-j05 "j05"
step monitor 6 report-j06 "j06"
step monitor 7 reconcile ""
step monitor 8 approve ""
step monitor 9 seal ""
collect monitor
close_role monitor
text "${pane[shell]}" exit
enter "${pane[shell]}"
: >"$WORK/answer.txt"
printf '%s\n' 'j01 status='"${status[j01]}"' receipt='"${result[j01]}" >>"$WORK/answer.txt"
printf '%s\n' 'j02 status='"${status[j02]}"' receipt='"${result[j02]}" >>"$WORK/answer.txt"
printf '%s\n' 'j03 status='"${status[j03]}"' receipt='"${result[j03]}" >>"$WORK/answer.txt"
printf '%s\n' 'j04 status='"${status[j04]}"' receipt='"${result[j04]}" >>"$WORK/answer.txt"
printf '%s\n' 'j05 status='"${status[j05]}"' receipt='"${result[j05]}" >>"$WORK/answer.txt"
printf '%s\n' 'j06 status='"${status[j06]}"' receipt='"${result[j06]}" >>"$WORK/answer.txt"
printf '%s\n' 'monitor='"${result[monitor]}" >>"$WORK/answer.txt"
