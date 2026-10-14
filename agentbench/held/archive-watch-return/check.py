
import json
import sys
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        def rec(name):
            actual = json.loads((w / '.fx' / (name + '.json')).read_text())
            assert actual.pop('terminal') == {'stdin_tty': True, 'stdout_tty': True}, 'fixture did not run in a terminal'
            return actual
        def exact(name, expected):
            actual = rec(name)
            assert json.dumps(actual, sort_keys=True) == json.dumps(expected, sort_keys=True), f'{name}: unexpected record'
        def answer(expected):
            assert (w / 'answer.txt').read_text() == expected, 'answer.txt differs'
        import hashlib
        d = lambda s: hashlib.sha256(s.encode()).hexdigest()[:12]
        t = d('archive-watch:epoch-3')
        m = d(t+':maintenance-complete')
        r = d(m+':same-process')
        p = rec('archive.parked')
        assert type(p['pid']) is int and p['pid'] > 0, 'missing watcher identity'
        exact('archive.parked', {'ticket': t, 'received': 'park '+t, 'pid': p['pid']})
        exact('archive.maintenance', {'receipt': m, 'watch_pid': p['pid'], 'state': 'complete'})
        exact('archive.resumed', {'ticket': t, 'maintenance': m, 'resume': r, 'received': 'resume '+r, 'pid': p['pid']})
        exact('archive.stopped', {'received': 'stop '+t, 'ticket': t, 'pid': p['pid']})
        answer(f'ticket={t}\nmaintenance={m}\nresume={r}\n')
        return True, 'all required outcomes verified'
    except (OSError, ValueError, AssertionError, KeyError, TypeError) as e:
        return False, str(e)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
