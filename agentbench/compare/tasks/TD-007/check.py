import json, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        seed = 'archive/extent/271'
        assert (w / 'seed.txt').read_text() == 'archive/extent/271\n', 'setup seed changed'
        def token(tag): return hashlib.sha256((seed + '|' + tag).encode()).hexdigest()[:12].upper()
        def record(name):
            r = json.loads((w / '.fx' / (name + '.success.json')).read_text())
            assert r.get('terminal') is True and isinstance(r.get('pid'), int) and r.get('tty'), 'terminal evidence missing'
            return r
        r = record('archive')
        assert r['query'] == 'report VOLUME-K9'
        for k in ('origin', 'digest', 'ticket'): assert r[k] == token(k)
        assert (w / 'answer.txt').read_bytes() == ''.join(k + '=' + token(k) + '\n' for k in ('origin', 'digest', 'ticket')).encode(), 'incorrect archive values'
        return True, 'exact answer and terminal protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'final state does not match'
