#!/usr/bin/env python3
"""See README.md for the fixture's oracle ground truth."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from oracle_support import Editor, arguments, run

def configure(p):
    p.add_argument('--jobs', type=int, default=2)
    p.add_argument('--compute', action='store_true')

def body(t, a, rng):
    delay = rng.uniform(0.18, 0.23)
    iterations = rng.randrange(1800000, 2100000)
    for i in range(a.jobs):
        t.write('\x1b[36mjob[%d]> \x1b[0m' % i, answers=t.consumed if i else None)
        if a.compute:
            t.event('block', on='work')
            checksum = 0
            for number in range(iterations):
                checksum = (checksum + number * number) % 1000003
            t.write('[scan %d complete] ' % checksum)
        else:
            t.sleep(delay)
            t.write('[scan complete] ')
        data = t.read().rstrip(b'\n')
        # One syscall draws and erases intermediate status before the final reply.
        t.write(b'\rscanning ' + data + b'...\r\x1b[2K\x1b[5Cqueued: ' + data + b'\r\n', answers=t.consumed)

args = arguments('Busy canonical console accepts typed-ahead jobs.', configure)
run('FX-09', args, body)
