#!/usr/bin/env bash
set -euo pipefail
python3 - "$@" <<'PYREF'
import os, sys, time, re, shlex, subprocess
from pathlib import Path
work, fixtures = map(lambda x: Path(x).resolve(), sys.argv[1:3])
def tm(*args):
    return subprocess.check_output(['tmux', '-L', 'ab', *map(str, args)], text=True).strip()
def send(pane, text):
    tm('send-keys', '-t', pane, '-l', text)
    tm('send-keys', '-t', pane, 'Enter')
def wait(log, pattern):
    end = time.monotonic() + 90
    while time.monotonic() < end:
        try: data = log.read_bytes().decode('utf-8', 'replace')
        except FileNotFoundError: data = ''
        m = re.search(pattern, data)
        if m: return m
        time.sleep(.025)
    raise RuntimeError('did not observe ' + pattern + '\n' + data[-2000:])
def attach(pane, name):
    log = work / ('trace-' + name + '.log')
    log.write_text('')
    tm('pipe-pane', '-O', '-t', pane, 'cat >> ' + shlex.quote(str(log)))
    cmd = 'python3 -I ' + shlex.quote(str(fixtures / (name + '.py'))) + ' ' + shlex.quote(str(work))
    send(pane, cmd)
    return log
def answer(lines):
    (work / 'answer.txt').write_text(''.join(s + '\n' for s in lines))
tm('new-session', '-d', '-s', 'task', '-x', '120', '-y', '35')
tm('set-option', '-g', 'history-limit', '12000')
pane = tm('display-message', '-p', '-t', 'task:0', '#{pane_id}')

probe_pane = tm('split-window', '-d', '-v', '-t', pane, '-P', '-F', '#{pane_id}')
serve_pane = tm('split-window', '-d', '-h', '-t', probe_pane, '-P', '-F', '#{pane_id}')
build_log = attach(pane, 'compile')
probe_log = attach(probe_pane, 'probe')
serve_log = attach(serve_pane, 'serve')
wait(build_log, r'COMPILER INPUT> ')
wait(probe_log, r'PROBE READY> ')
wait(serve_log, r'SERVER READY> ')
send(pane, 'build READY')
artifact = wait(build_log, r'BUILD artifact=([A-F0-9]{12})').group(1)
wait(build_log, r'COMPILER CLOSED')
send(probe_pane, 'test ' + artifact)
seal = wait(probe_log, r'VERDICT seal=([A-F0-9]{12})').group(1)
wait(probe_log, r'FINISH INPUT: after the server has stopped, enter finish> ')
send(serve_pane, 'publish ' + seal)
deployment = wait(serve_log, r'DEPLOY id=([A-F0-9]{12})').group(1)
wait(serve_log, r'STOP INPUT: enter stop <id> using the DEPLOY id> ')
send(serve_pane, 'stop ' + deployment)
wait(serve_log, r'SERVER CLOSED')
send(probe_pane, 'finish')
wait(probe_log, r'PROBE CLOSED')
answer(['artifact=' + artifact, 'seal=' + seal, 'deployment=' + deployment])
PYREF
