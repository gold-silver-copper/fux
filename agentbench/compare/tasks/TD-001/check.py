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
        d = record(work, 'dispatch')
        assert d['received'] == ['dispatch ' + code('dispatch-job'), 'seal ' + code('dispatch-challenge')], 'incorrect submitted commands'
        assert d['dispatch'] == code('dispatch-job') and d['seal'] == code('dispatch-final'), 'incorrect committed dispatch'
        answer(work, 'dispatch=' + code('dispatch-job') + '\nseal=' + code('dispatch-final') + '\n')
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as exc:
        return False, str(exc)
