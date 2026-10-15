#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--reads', type=int, default=3)
    p.add_argument('--vmin', type=int, choices=[0, 3], default=0)
    p.add_argument('--vtime', type=int, choices=[1, 2, 3], default=None)

def body(t, a, rng):
    timeout = a.vtime if a.vtime is not None else rng.choice([1, 2])
    t.set_mode(raw=True, echo=False, vmin=a.vmin, vtime=timeout)
    t.write('Serial sampler VMIN=%d VTIME=%d\r\n' % (a.vmin, timeout))
    for i in range(a.reads):
        t.write('packet[%d]> ' % i, answers=t.consumed if i else None)
        data = t.read(32)
        if data:
            t.write(data, role='echo', answers=t.consumed)
            t.write(b'\r\nreceived ' + str(len(data)).encode() + b' bytes: ' + data + b'\r\n', answers=t.consumed)
        else:
            t.write('\r\nserial timeout (empty read)\r\n')

args = arguments('Noncanonical VMIN/VTIME serial reads.', configure)
run('FX-06', args, body)
