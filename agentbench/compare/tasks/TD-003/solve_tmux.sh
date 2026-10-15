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
# Give the interactive shell a distinctive prompt before invoking the build.
text("PS1='REFSHELL> '")
keys('Enter')
wait(r'^REFSHELL> *$')
launch('build', '"$-"')
draft = wait(r'^BUSY DRAFT: type exactly this WITHOUT Enter: (note [A-F0-9]+ status=[0-9]+)$').group(1)
text(draft)
wait(r'^NOTE READY:')
keys('Enter')
note = wait(r'^NOTE ACKNOWLEDGED id=([A-F0-9]+)$').group(1)
artifact = wait(r'^PUBLISHED ARTIFACT id=([A-F0-9]+)$').group(1)
# This prompt can only be the one following the completed build: earlier prompts contain commands.
wait(r'^REFSHELL> *$')
launch('status', '"$?" "$-"')
status = wait(r'^SHELL STATUS RECORDED status=([0-9]+)$').group(1)
write('artifact=' + artifact + '\nnote=' + note + '\nstatus=' + status + '\n')

PY
