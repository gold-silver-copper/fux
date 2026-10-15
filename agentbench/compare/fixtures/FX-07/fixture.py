#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--commands', type=int, default=1)
    p.add_argument('--idle-ticks', type=int, default=12)

def body(t, a, rng):
    t.set_mode(raw=True, echo=False)
    delay = rng.uniform(0.065, 0.085)
    t.write('Queue watcher (polling input)\r\nwatch> ')
    buf, idle, commands = b'', 0, 0
    while commands < a.commands and idle < a.idle_ticks:
        if not t.poll(delay):
            idle += 1
            t.write('\r\x1b[2K[queue idle %d]\r\n' % idle)
            t.write(b'watch> ' + buf, role='echo' if buf else 'output')
            if idle == 2:
                t.bell()
            continue
        key = t.read(1)
        idle = 0
        if key in (b'\r', b'\n'):
            t.write(b'\r\n', role='echo', answers=t.consumed)
            t.bell(b'Enqueued: ' + buf + b'\r\n', answers=t.consumed)
            commands += 1
            buf = b''
            if commands < a.commands:
                t.write('watch> ', answers=t.consumed)
        elif key in (b'\x7f', b'\x08'):
            buf = buf[:-1]
            t.write(b'\r\x1b[2Kwatch> ' + buf, role='echo', answers=t.consumed)
        elif key == b'\x07':
            t.bell(answers=t.consumed)
        elif 32 <= key[0] < 127:
            buf += key
            t.write(key, role='echo', answers=t.consumed)
        else:
            t.bell(answers=t.consumed)
    if idle >= a.idle_ticks:
        t.write('\r\nWatcher idle limit reached.\r\n')

args = arguments('Select timeout, heartbeat redraw, and bells.', configure)
run('FX-07', args, body)
