import hashlib
import json
from pathlib import Path

def code(label):
    return hashlib.sha256(('echo-lab-v3:' + label).encode()).hexdigest()[:9].upper()

def record(work, name):
    data = json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    assert data.get('tty_in') is True and data.get('tty_out') is True, 'fixture did not run in a terminal'
    assert data.get('complete') is True and 'error' not in data, 'fixture did not complete successfully'
    return data

def answer(work, expected):
    assert (Path(work) / 'answer.txt').read_bytes() == expected.encode(), 'answer.txt does not match the observed results exactly'

def check(work: str) -> tuple[bool, str]:
    try:
        d = record(work, 'routing')
        assert d['received'] == ['prepare', 'commit ' + code('routing-commit')], 'review/commit protocol incomplete'
        assert d['recipient'] == 'night-ops' and d['mode'] == 'guarded' and d.get('clean_exit') is True, 'form or exit state incorrect'
        assert d['revision'] == code('routing-revision') and d['receipt'] == code('routing-receipt'), 'incorrect saved routing'
        answer(work, 'revision=' + code('routing-revision') + '\nreceipt=' + code('routing-receipt') + '\n')
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as exc:
        return False, str(exc)
