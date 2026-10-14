import os, sys
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fxlib import Fixture
import hashlib
import select
import signal
mode = sys.argv[2]
f = Fixture(sys.argv[1], mode)
queue = tuple('cedar relay drain seed'.split()) + ('e', 'f', 'g')
drained = len(queue)
loader_status = 128 + signal.SIGINT
window = False
count = 0
def interrupted(signum, frame):
    global count
    count += 1
    if not window:
        f.fail('interrupt before ready window')
    if mode == 'loader':
        f.out('\nLOADER CANCELLED during safe cancellation window')
        f.save(status='complete', exit_code=loader_status, signal='SIGINT', window='open', interrupts=count)
        raise SystemExit(loader_status)
    f.out('\nSIGINT ignored: relay must drain queued records')
signal.signal(signal.SIGINT, interrupted)
if mode == 'loader':
    f.out('Loader: opening immutable input...')
    f.guard(4)
    window = True
    f.out('CANCELLATION WINDOW OPEN: Ctrl-C cancels safely')
    while True:
        if select.select([sys.stdin], [], [], .1)[0]:
            f.fail('loader accepts a signal, not text')
elif mode == 'relay':
    f.out('Relay: recovering queue...')
    f.guard(3)
    window = True
    f.out('RELAY READY: queued=%d; interrupt is ignored' % drained)
    while count == 0:
        if select.select([sys.stdin], [], [], .1)[0]:
            f.fail('relay requires an interrupt before drain')
    token = hashlib.sha256(b'relay seven records drain').hexdigest()[:10]
    f.ask('DRAIN token=' + token + ' > ', 'drain ' + token)
    for record in range(1, drained + 1):
        f.guard(1)
        f.out('Drained %d/%d' % (record, drained))
    if count != 1:
        f.fail('expected exactly one ignored interrupt')
    f.out('RELAY STOPPED cleanly drained=%d ignored=%d' % (drained, count))
    f.finish(ignored=count, drained=drained, shutdown='clean')
else:
    f.fail('unknown service')
