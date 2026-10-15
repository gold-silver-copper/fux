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
        b = record(work, 'build')
        s = record(work, 'status')
        expected = 17 + int(code('build-exit')[:2], 16) % 73
        assert b.get('interactive') and s.get('interactive') and s.get('after_build'), 'interactive shell/order not verified'
        assert s['build_finished_ns'] == b['finished_ns'], 'status belongs to a different build attempt'
        assert b['received'] == ['note ' + code('build-note') + ' status=' + str(int(code('build-decoy')[:2], 16))], 'wrong queued note'
        assert b['exit_status'] == expected and s['status'] == expected and s['received'] == [str(expected)], 'wrong shell exit status'
        assert b['artifact'] == code('build-artifact') and b['note'] == code('build-note'), 'wrong build result'
        answer(work, 'artifact=' + code('build-artifact') + '\nnote=' + code('build-note') + '\nstatus=' + str(expected) + '\n')
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as exc:
        return False, str(exc)
