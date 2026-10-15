#!/usr/bin/env bash
set -euo pipefail
work=$(cd "$1" && pwd)
fixtures=$(cd "$2" && pwd)
session=reference
tmux -L ab -f /dev/null new-session -d -s "$session" -x 72 -y 24 'bash --noprofile --norc -i'
trap 'tmux -L ab kill-session -t "$session" 2>/dev/null || true' EXIT
tmux -L ab set-option -g status off
tmux -L ab set-option -g history-limit 100000
pane="$session:0.0"
log="$work/terminal.log"
printf -v quoted_log '%q' "$log"
tmux -L ab pipe-pane -t "$pane" "cat > $quoted_log"
send() {
    tmux -L ab send-keys -t "$pane" -l -- "$1"
    tmux -L ab send-keys -t "$pane" Enter
}
wait_match() {
    python3 - "$log" "$1" <<'PY'
import pathlib, re, sys, time
p, pattern = pathlib.Path(sys.argv[1]), sys.argv[2]
until = time.monotonic() + 120
while time.monotonic() < until:
    text = p.read_text(errors='replace') if p.exists() else ''
    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text).replace('\r', '\n')
    m = re.search(pattern, text, re.M)
    if m and m.end() < len(text):
        print('\t'.join(m.groups()) if m.groups() else m.group())
        break
    time.sleep(.03)
else:
    raise SystemExit('No terminal output matching: ' + pattern)
PY
}
launch() {
    local cmd
    printf -v cmd 'python3 -I %q %q' "$fixtures/$1" "$work"
    if [[ $# -gt 1 ]]; then
        local arg
        for arg in "${@:2}"; do
            printf -v cmd '%s %q' "$cmd" "$arg"
        done
    fi
    send "$cmd"
}

launch ledger.py
scan=$(wait_match 'INPUT READY type scan ([a-f0-9]+)')
send "scan $scan"
header=$(wait_match '^HEADER key=([a-f0-9]+)')
tail=$(wait_match '^ROW-END [^\n]* tail=([a-f0-9]+)')
wait_match '^RESIZE NOW:' >/dev/null
tmux -L ab resize-window -t "$session:0" -x 132 -y 32
receipt=$(wait_match '^RESIZE RECEIPT ([a-f0-9]+)')
wait_match 'INPUT READY type commit' >/dev/null
send "commit $header $tail $receipt"
wait_match 'LEDGER CLOSED' >/dev/null
printf 'header=%s\ntail=%s\nresize=%s\n' "$header" "$tail" "$receipt" > "$work/answer.txt"
