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
launch('intake')
wait(r'^draft> *$')
draft = wait(r'^(intake [A-F0-9]+ receipt=[A-F0-9]+)$').group(1)
text(draft)
wait(r'^CORRECTION READY:')
s = screen()
corrected = re.search(r'CORRECTION READY:[^\n]*\n(intake [A-F0-9]+ receipt=[A-F0-9]+)', s).group(1)
keys('C-u')
text(corrected)
keys('Enter')
confirm = wait(r'^CONFIRM READY: submit exactly (confirm [A-F0-9]+)$').group(1)
text(confirm)
keys('Enter')
ident = wait(r'^INTAKE ACCEPTED id=([A-F0-9]+)$').group(1)
receipt = wait(r'^ISSUED RECEIPT value=([A-F0-9]+)$').group(1)
write('intake=' + ident + '\nreceipt=' + receipt + '\n')

PY
