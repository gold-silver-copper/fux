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
launch('dispatch')
wait(r'^READY DISPATCH EDITOR$')
job = wait(r'^Replace the draft with exactly: dispatch ([A-F0-9]+)$').group(1)
# Exercise deletion, word deletion, cursor moves and a full buffer clear.
keys('End', 'BSpace', 'C-w', 'Home', 'Delete', 'Right', 'Left', 'End', 'C-u')
text('dispatch ' + job)
keys('Enter')
challenge = wait(r'^READY SEAL: submit exactly seal ([A-F0-9]+)$').group(1)
text('seal ' + challenge)
keys('Enter')
job = wait(r'^ACCEPTED DISPATCH id=([A-F0-9]+)$').group(1)
seal = wait(r'^SERVER SEALED value=([A-F0-9]+)$').group(1)
write('dispatch=' + job + '\nseal=' + seal + '\n')

PY
