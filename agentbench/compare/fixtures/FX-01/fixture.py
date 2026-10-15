#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--lines', type=int, default=2)
    p.add_argument('--reply', choices=['prefix', 'suffix', 'exact'], default='prefix')

def body(t, a, rng):
    t.write('\x1b[34mArchive search console\x1b[0m\r\n')
    for i in range(a.lines):
        t.write('query> ', answers=t.consumed if i else None)
        data = t.read().rstrip(b'\n')
        t.sleep(rng.uniform(0.015, 0.035))
        if a.reply == 'prefix':
            reply = data + b': found in archive'
        elif a.reply == 'suffix':
            reply = b'Archive result: ' + data
        else:
            reply = data
        t.write(b'\r', answers=t.consumed)
        t.write(reply + b'\r\n', answers=t.consumed)

args = arguments('Canonical archive console with kernel echo.', configure)
run('FX-01', args, body)
