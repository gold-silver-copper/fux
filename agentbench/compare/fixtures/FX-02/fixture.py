#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--attempts', type=int, default=1)

def body(t, a, rng):
    t.write('Secure vault sign-in\r\n')
    for attempt in range(a.attempts):
        t.set_mode(raw=False, echo=True)
        t.write('User: ', answers=t.consumed if attempt else None)
        user = t.read().rstrip(b'\n')
        t.set_mode(raw=False, echo=False)
        t.write('\r\x1b[33mPassword: \x1b[0m', answers=t.consumed)
        password = t.read().rstrip(b'\n')
        t.sleep(rng.uniform(0.015, 0.035))
        # No leading newline: the hidden submission's reply starts on the prompt row.
        if password == b'swordfish':
            t.write(b'Unlocked for ' + user + b'.\r\n', answers=t.consumed)
        else:
            t.bell(b'Access denied.\r\n', answers=t.consumed)
        t.set_mode(raw=False, echo=True)

args = arguments('Canonical password prompt with echo disabled.', configure)
run('FX-02', args, body)
