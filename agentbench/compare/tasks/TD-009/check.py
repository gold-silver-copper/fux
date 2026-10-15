import json, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        seed = 'migration/north/144'
        assert (w / 'seed.txt').read_text() == 'migration/north/144\n', 'setup seed changed'
        def token(tag): return hashlib.sha256((seed + '|' + tag).encode()).hexdigest()[:12].upper()
        def record(name):
            r = json.loads((w / '.fx' / (name + '.success.json')).read_text())
            assert r.get('terminal') is True and isinstance(r.get('pid'), int) and r.get('tty'), 'terminal evidence missing'
            return r
        r = record('migration')
        assert r['recipe'] == ['route north', 'message You said: COMMIT', '', 'limit 017', '.']
        assert r['finalize'] == 'finalize ' + token('challenge') and r['checksum'] == token('checksum')
        assert (w / 'answer.txt').read_bytes() == ('checksum=' + token('checksum') + '\n').encode(), 'incorrect migration checksum' 
        return True, 'exact answer and terminal protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'final state does not match'
