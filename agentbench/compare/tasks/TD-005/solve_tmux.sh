#!/usr/bin/env bash
set -euo pipefail
python3 - "$1" "$2" <<'PY'
import os, re, shlex, subprocess, sys, time
from pathlib import Path
work, fixtures = map(os.path.abspath, sys.argv[1:])
def tm(*args):
    return subprocess.check_output(['tmux', '-L', 'ab', *args], stderr=subprocess.STDOUT).decode()
tm('new-session', '-d', '-s', 'ref', '-x', '140', '-y', '45', 'bash --noprofile --norc -i')
pane = 'ref:0.0'
def screen():
    return tm('capture-pane', '-p', '-S', '-200', '-t', pane)
def wait(pattern):
    until = time.monotonic() + 90
    while time.monotonic() < until:
        s = screen()
        m = re.search(pattern, s, re.M)
        if m:
            return m
        time.sleep(0.04)
    raise RuntimeError('missing screen pattern: ' + pattern + '\n' + screen())
def text(s):
    tm('send-keys', '-t', pane, '-l', '--', s)
def keys(*ks):
    tm('send-keys', '-t', pane, *ks)
def launch(name, args=''):
    text('python3 -I ' + shlex.quote(fixtures + '/' + name + '.py') + ' ' + shlex.quote(work) + (' ' + args if args else ''))
    keys('Enter')
def write(s):
    Path(work, 'answer.txt').write_text(s)
launch('routing')
wait(r'^FORM READY$')
keys('C-u')
text('night-ops')
keys('Tab', 'Down', 'F2')
wait(r'^REVIEW READY: submit exactly prepare$')
text('prepare')
keys('Enter')
token = wait(r'^COMMIT READY: submit exactly commit ([A-F0-9]+)$').group(1)
text('commit ' + token)
keys('Enter')
wait(r'^ROUTING COMMITTED$')
revision = wait(r'^SAVED REVISION value=([A-F0-9]+)$').group(1)
receipt = wait(r'^SAVED RECEIPT value=([A-F0-9]+)$').group(1)
wait(r'^EXIT READY: press q to exit\.$')
text('q')
wait(r'^Routing editor exited successfully\.$')
write('revision=' + revision + '\nreceipt=' + receipt + '\n')

PY
