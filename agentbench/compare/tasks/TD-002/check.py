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
        d = record(work, 'intake')
        assert d.get('canonical_echo') is True, 'intake did not use canonical echo'
        assert d['received'] == ['intake ' + code('intake-id') + ' receipt=' + code('intake-correction-tag'), 'confirm ' + code('intake-confirm')], 'incorrect final submitted lines'
        assert d['receipt'] == code('intake-issued') and d['intake'] == code('intake-id'), 'incorrect intake result'
        answer(work, 'intake=' + code('intake-id') + '\nreceipt=' + code('intake-issued') + '\n')
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as exc:
        return False, str(exc)
