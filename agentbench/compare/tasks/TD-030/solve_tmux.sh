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

build_pane="$pane"
build_log="$log"
launch pipeline.py build
challenge=$(wait_match 'INPUT READY type build ([a-f0-9]+)')
send "build $challenge"
artifact=$(wait_match '^ARTIFACT ([a-f0-9]+)')
wait_match '^BUILD HOLDING' >/dev/null
wait_match 'INPUT READY after TEST CLOSED' >/dev/null

pane=$(tmux -L ab split-window -h -t "$build_pane" -P -F '#{pane_id}' 'bash --noprofile --norc -i')
test_pane="$pane"
log="$work/test-terminal.log"
test_log="$log"
printf -v quoted_log '%q' "$log"
tmux -L ab pipe-pane -t "$pane" "cat > $quoted_log"
launch pipeline.py test
wait_match 'INPUT READY type test' >/dev/null
send "test $artifact"
verdict_result=$(wait_match 'TEST ACCEPTED[\s\S]*?VERDICT ([A-Z]+) seal=([a-f0-9]+)')
IFS=$'\t' read -r verdict seal <<< "$verdict_result"
wait_match '^TEST HOLDING' >/dev/null
wait_match 'INPUT READY after PUBLISH CLOSED' >/dev/null

pane=$(tmux -L ab split-window -v -t "$test_pane" -P -F '#{pane_id}' 'bash --noprofile --norc -i')
log="$work/serve-terminal.log"
printf -v quoted_log '%q' "$log"
tmux -L ab pipe-pane -t "$pane" "cat > $quoted_log"
launch pipeline.py serve
wait_match 'INPUT READY type serve' >/dev/null
send "serve $artifact $seal"
binding=$(wait_match 'SERVE ACCEPTED[\s\S]*?BOUND endpoint=(local-[a-z0-9-]+) ticket=([a-f0-9]+)')
IFS=$'\t' read -r endpoint bound <<< "$binding"
wait_match 'INPUT READY type publish' >/dev/null
send "publish $artifact $seal $bound"
publication=$(wait_match '^PUBLICATION ([a-f0-9]+)')
wait_match '^PUBLISH CLOSED' >/dev/null

pane="$test_pane"
log="$test_log"
send "stop $seal"
wait_match '^TEST CLOSED' >/dev/null
pane="$build_pane"
log="$build_log"
send "stop $artifact"
wait_match '^BUILD CLOSED' >/dev/null
printf 'artifact=%s\ntest=%s/%s\nendpoint=%s/%s\npublication=%s\n' "$artifact" "$verdict" "$seal" "$endpoint" "$bound" "$publication" > "$work/answer.txt"
