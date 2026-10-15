#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--lines', type=int, default=1)
    p.add_argument('--reply', choices=['prefix', 'suffix', 'exact'], default='prefix')

def body(t, a, rng):
    t.set_mode(raw=True, echo=False)
    t.write('Tiny shell: edit a command, then Enter.\r\n')
    for _ in range(a.lines):
        line = Editor(t, 'shell$ ').line()
        t.sleep(rng.uniform(0.005, 0.015))
        reply = line + ' -> queued' if a.reply == 'prefix' else ('queued: ' + line if a.reply == 'suffix' else line)
        t.write(reply + '\r\n', answers=t.consumed)

args = arguments('Readline-like raw ASCII editor.', configure)
run('FX-03', args, body)
