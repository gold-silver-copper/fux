import os, sys
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fxlib import Fixture
import hashlib
import select
f = Fixture(sys.argv[1], 'audit_view')
archive = hashlib.sha256(b'sealed archive 41').hexdigest()[:16]
wide = hashlib.sha256(b'wide index row 779').hexdigest()[:18]
queue_seed = b'consistent queue snapshot 1017'
frozen_queue = int(queue_seed.split()[-1])
f.out('Audit reader: immutable rows, followed by a live queue monitor')
f.guard(2)
for row in range(2200):
    if row == 41:
        f.out('ROW 0041 SEALED ARCHIVE = \x1b[32m' + archive + '\x1b[0m')
    elif row == 779:
        f.out('ROW 0779 WIDE INDEX ' + ('segment:' + 'x' * 24 + '|') * 10 + ' CHECKSUM=' + wide)
    elif row in (1300, 2000):
        f.out('ROW %04d ARCHIVE candidate = 0000000000000000 (unsealed)' % row)
    else:
        f.out('ROW %04d copied=%d bucket=%02d verified' % (row, (row * 37) % 811, row % 23))
    if row % 250 == 249:
        f.guard(.15)
f.out('LIVE MONITOR: type freeze to take the final consistent snapshot')
for value in (900, 940, 988, 1004):
    f.out('\r\x1b[2Kqueue=%d provisional' % value, end='')
    ready, _, _ = select.select([sys.stdin], [], [], .8)
    if ready:
        f.response('freeze')
        break
else:
    f.out('\n')
    f.ask('queue=1004 provisional; freeze > ', 'freeze')
f.out('\r\x1b[2KFROZEN queue=' + str(frozen_queue))
f.out('Snapshot stable. Audit reader closed.')
f.finish(archive=archive, wide=wide, queue=frozen_queue)
