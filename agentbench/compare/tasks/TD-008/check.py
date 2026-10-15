import json, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        seed = 'vault/watch/59'
        assert (w / 'seed.txt').read_text() == 'vault/watch/59\n', 'setup seed changed'
        def token(tag): return hashlib.sha256((seed + '|' + tag).encode()).hexdigest()[:12].upper()
        def record(name):
            r = json.loads((w / '.fx' / (name + '.success.json')).read_text())
            assert r.get('terminal') is True and isinstance(r.get('pid'), int) and r.get('tty'), 'terminal evidence missing'
            return r
        r = record('watch')
        assert r['password'] == token('password') and r['watch'] == 'watch READY'
        assert r['ack'] == 'ack ' + token('ticket')
        assert r['session'] == token('session') and r['receipt'] == token('receipt')
        assert (w / 'answer.txt').read_bytes() == ('session=' + token('session') + '\nreceipt=' + token('receipt') + '\n').encode(), 'incorrect watch values' 
        return True, 'exact answer and terminal protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'final state does not match'
