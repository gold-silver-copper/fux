#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--lines', type=int, default=1)

def body(t, a, rng):
    t.set_mode(raw=True, echo=False)
    t.write('Command palette\r\n')
    for _ in range(a.lines):
        line = Editor(t, 'action> ', area=True).line()
        t.sleep(rng.uniform(0.005, 0.015))
        t.write('Executing: ' + line + '\r\n', answers=t.consumed)

args = arguments('Three-row prompt-toolkit-like completion area.', configure)
run('FX-04', args, body)
