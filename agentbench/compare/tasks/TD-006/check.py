import json, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        seed = 'freight/ledger/83'
        assert (w / 'seed.txt').read_text() == 'freight/ledger/83\n', 'setup seed changed'
        def token(tag): return hashlib.sha256((seed + '|' + tag).encode()).hexdigest()[:12].upper()
        def record(name):
            r = json.loads((w / '.fx' / (name + '.success.json')).read_text())
            assert r.get('terminal') is True and isinstance(r.get('pid'), int) and r.get('tty'), 'terminal evidence missing'
            return r
        r = record('receipt')
        assert r['query'] == 'lookup crate/west-7'
        assert r['receipt'] == token('receipt') and r['seal'] == token('seal')
        assert (w / 'answer.txt').read_bytes() == ('receipt=' + token('receipt') + '\nseal=' + token('seal') + '\n').encode(), 'incorrect receipt or seal' 
        return True, 'exact answer and terminal protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'final state does not match'
