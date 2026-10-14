
from pathlib import Path
import hashlib
import json
import sys

def digest(s, n=12):
    return hashlib.sha256(s.encode()).hexdigest()[:n]

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    f = w / '.fx'
    def read(name):
        return json.loads((f / name).read_text())
    def ran(name, code=0):
        assert (f / (name + '.starts')).read_text() == 'start\n', name + ' was restarted'
        assert int((f / (name + '.pid')).read_text()) > 1
        assert read(name + '.exit.json') == {'code': code}, name + ' exit status'
        terminal = read(name + '.terminal.json')
        assert terminal['stdin'].startswith('/dev/') and terminal['stdin'] == terminal['stdout'], name + ' terminal identity'
    try:
        assert not list(f.glob('*.error.json')), 'fixture rejected an action'
        for name in ('builder', 'test_runner', 'preview'): ran(name)
        assert len({read(name + '.terminal.json')['stdin'] for name in ('builder', 'test_runner', 'preview')}) == 3, 'three distinct terminals required'
        build = 'B-' + digest('widget=quartz\nversion=4\n')
        tests = 'T-' + digest(build + '|27-passed')
        pair = build + ' ' + tests
        ticket = 'S-' + digest(pair + '|snapshot')
        assert read('build.json') == {'token': build, 'companions_alive': True}
        assert read('tests.json') == {'build': build, 'signature': tests, 'passed': 27}
        assert read('preview.json') == {'pair': pair, 'action': 'snapshot', 'ticket': ticket, 'shutdown': 'shutdown ' + ticket, 'closed': True}
        assert (w / 'answer.txt').read_text() == f'build={build}\ntests={tests}\nsnapshot={ticket}\n'

        return True, 'exact answer and fixture records match'
    except OSError:
        return False, 'missing required record, answer or live shell'
    except (ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'incomplete or incorrect state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
