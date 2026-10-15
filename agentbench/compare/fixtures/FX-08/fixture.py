#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

import threading

def configure(p):
    p.add_argument('--ticks', type=int, default=5)

def body(t, a, rng):
    t.set_mode(raw=True, echo=False)
    delays = [rng.uniform(0.045, 0.07) for _ in range(a.ticks)]
    editor = Editor(t, 'search> ')
    stop = threading.Event()
    editor.paint('output')
    def worker():
        for i, delay in enumerate(delays):
            t.event('block', on='sleep')
            if stop.wait(delay):
                return
            with editor.guard:
                t.write('\r\x1b[2K\x1b[35m[indexer] shard %d complete\x1b[0m\r\n' % (i+1), thread='indexer')
                editor.paint(answers=None, thread='indexer')
    thread = threading.Thread(target=worker, name='indexer')
    thread.start()
    try:
        line = editor.line(already_painted=True)
    finally:
        stop.set()
        t.event('block', on='sleep')
        thread.join()
    t.write('Search matches for ' + line + '\r\n', answers=t.consumed)

args = arguments('Raw editor with another thread printing during typing.', configure)
run('FX-08', args, body)
