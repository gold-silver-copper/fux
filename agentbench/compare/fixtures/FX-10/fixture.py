#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--width', type=int, default=180)
    p.add_argument('--rows', type=int, default=40)

def body(t, a, rng):
    t.write('Build report generator\r\nLabel: ')
    label = t.read().rstrip(b'\n')
    t.sleep(rng.uniform(0.01, 0.025))
    t.bell(answers=t.consumed)
    # Transient progress and final status are deliberately in the SAME write.
    t.write(b'phase one...\rphase two...\x1b[1G\x1b[2Kreport for ' + label + b'\r\n', answers=t.consumed)
    alphabet = 'abcdefghijklmnopqrstuvwxyz0123456789'
    for row in range(a.rows):
        text = ''.join(rng.choice(alphabet) for _ in range(a.width))
        t.write('\x1b[32m%03d\x1b[0m %s\r\n' % (row, text), answers=t.consumed)
    t.bell(b'Build complete: ' + label + b'\r\n', answers=t.consumed)

args = arguments('Wrapped lines, scrolling, bells, and transient write bursts.', configure)
run('FX-10', args, body)
