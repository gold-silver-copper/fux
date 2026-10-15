#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def body(t, a, rng):
    t.set_mode(raw=True, echo=False)
    t.alternate(True)
    t.write('\x1b[?25l\x1b[2J\x1b[H\x1b[1;34mBuild dashboard\x1b[0m\r\n')
    t.write('j/k or arrows select | Enter inspect | type notes | q quit\r\n')
    selected, note = 0, ''
    key_reader = Editor(t)
    def rows(answers):
        data = '\x1b[4;1H'
        for i, name in enumerate(['compile', 'unit tests', 'package']):
            data += '\x1b[2K' + ('\x1b[7m> ' if i == selected else '  ') + name + '\x1b[0m\r\n'
        t.write(data, answers=answers)
    rows(None)
    t.write('\x1b[9;1Hnote> ')
    while True:
        key = key_reader.key()
        n = t.consumed
        if key == b'q':
            break
        if key in (b'j', b'\x1b[B'):
            selected = (selected + 1) % 3
            rows(n)
        elif key in (b'k', b'\x1b[A'):
            selected = (selected - 1) % 3
            rows(n)
        elif key in (b'\r', b'\n'):
            t.sleep(rng.uniform(0.01, 0.03))
            t.write('\x1b[8;1H\x1b[2KDetails for job %d: ready' % (selected + 1), answers=n)
        elif key in (b'\x7f', b'\x08'):
            note = note[:-1]
            t.write('\x1b[9;1H\x1b[2Knote> ' + note, role='echo', answers=n)
        elif len(key) == 1 and 32 <= key[0] < 127:
            note += key.decode('ascii')
            t.write('\x1b[9;1H\x1b[2Knote> ' + note, role='echo', answers=n)
        else:
            t.bell(answers=n)
    t.write('\x1b[?25h')
    t.alternate(False)
    t.write('Dashboard closed.\r\n', answers=t.consumed)

args = arguments('Alternate-screen job dashboard.')
run('FX-05', args, body)
