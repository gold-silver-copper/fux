import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

import os
import signal

f = Fixture(sys.argv[1], 'ledger')
size = os.get_terminal_size(1)
f.r['initial_size'] = [size.columns, size.lines]
f.save()
if (size.columns, size.lines) != (72, 24):
    f.fail('start at exactly 72 columns by 24 rows')
resizing_allowed = False
early_resize = False
def changed(signum, frame):
    global early_resize
    if not resizing_allowed:
        early_resize = True
signal.signal(signal.SIGWINCH, changed)
header = token('ledger-live-header', 20)
tail = token('ledger-live-tail', 24)
scan = token('ledger-scan', 12)
f.out('\x1b[33mREHEARSAL HEADER key=' + token('ledger-old-header', 20) + '\x1b[0m')
f.out('REHEARSAL ROW-END tail=' + token('ledger-old-tail', 24))
f.out('Opening archive; no input or resizing yet.')
f.guard(4.6)
f.expect('type scan ' + scan, 'scan ' + scan)
f.out('SCAN ACCEPTED')
f.out('LEDGER LIVE BEGIN')
f.out('\x1b[36mHEADER key=' + header + '\x1b[0m')
bulk_bytes = 0
for block in range(40):
    for row in range(64):
        line = 'BULK %04d ' % (block * 64 + row) + '0123456789abcdef' * 32
        f.out(line)
        bulk_bytes += len(line) + 1
    f.guard(.045)
f.out('ROW-END ' + 'segment/' * 2000 + ' tail=' + tail)
f.out('LEDGER LIVE END')
f.out('\x1b[31mPREVIEW tail=' + token('ledger-preview-tail', 24) + '\x1b[0m', end='')
f.guard(.24)
f.out('\r\x1b[2KPreview dismissed.')
if early_resize or os.get_terminal_size(1) != size:
    f.fail('terminal was resized before RESIZE NOW')
resizing_allowed = True
f.out('RESIZE NOW: set this terminal to 132 columns by 32 rows; send no input yet.')
while True:
    geometry = os.get_terminal_size(1)
    if (geometry.columns, geometry.lines) == (132, 32):
        break
    f.guard(.05)
receipt = token('ledger-resize:' + header + ':' + tail + ':132x32', 18)
f.r.update(resized_size=[geometry.columns, geometry.lines], resize_after_live=True, bulk_bytes=bulk_bytes)
f.save()
f.out('RESIZE RECEIPT ' + receipt)
f.expect('type commit <header key> <tail key> <resize receipt>', 'commit ' + header + ' ' + tail + ' ' + receipt)
f.done(header=header, tail=tail, receipt=receipt, early_resize=early_resize)
f.out('LEDGER CLOSED')
