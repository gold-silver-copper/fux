import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

f = Fixture(sys.argv[1], 'shipping')
f.out('\x1b[35mHarbor shipping desk: cached decisions are not live\x1b[0m')
settlements = []
for parcel in (1, 2):
    p = 'parcel' + str(parcel)
    bid = token('shipping-bid-' + p, 12)
    first = ('REVIEW', 'CANCEL')[parcel - 1]
    seal = token('shipping-first-' + p, 16)
    f.out('DECISION ' + p + ' disposition=RELEASE seal=' + token('shipping-cache-' + p, 16))
    f.out('Loading desk policy. Do not submit a bid until INPUT READY.')
    f.guard(3.6 if parcel == 1 else 1.8)
    command = 'DECISION ' + p + ' disposition=RELEASE seal=' + bid
    f.expect(p + ': type exactly ' + command, command)
    f.out('BID ACCEPTED ' + p)
    f.out('DECISIONS OPEN ' + p)
    for tick in range(3):
        f.out('\r\x1b[2KReviewing ' + p + ' ' + '.' * (tick + 1), end='')
        f.guard((8.2 if parcel == 1 else 5.1) / 3)
    f.out('\r\x1b[2KDECISION ' + p + ' disposition=' + first + ' seal=' + seal, end='')
    f.guard(.34 if parcel == 1 else 1.3)
    f.out('\r\x1b[2KPolicy display refreshed', end='')
    f.guard(.65)
    f.out('\r\x1b[2KDECISION ' + p + ' disposition=RELEASE seal=' + token('shipping-late-' + p, 16))
    f.expect(p + ': type settle ' + p + ' <first disposition> <first seal>', 'settle ' + p + ' ' + first + ' ' + seal)
    settlements.append(first + '/' + seal)
    f.out('SETTLED ' + p)
manifest = token('shipping-manifest:' + ':'.join(settlements), 18)
f.done(settlements=settlements, manifest=manifest)
f.out('MANIFEST ' + manifest)
f.out('DESK CLOSED')
