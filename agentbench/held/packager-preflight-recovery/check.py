
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
        config = 'component=brass\nrevision=17\n'
        status = 1 + int(digest(config + '|preflight', 4), 16) % 63
        ran('packager.preflight', status)
        ran('packager.package')
        expected = {'cwd': str(w.resolve() / 'project'), 'flavor': 'release', 'cols': 96, 'rows': 26}
        assert read('packager.preflight.context.json') == expected
        assert read('packager.package.context.json') == expected
        config = 'component=brass\nrevision=17\n'
        artifact = 'brass-r17-' + digest(config + '|release')
        assert read('package.json') == {'key': digest(config + '|release|confirm', 10), 'artifact': artifact}
        assert (w / 'answer.txt').read_text() == f'preflight_exit={status}\npackage_exit=0\nartifact=' + artifact + '\n'

        return True, 'exact answer and fixture records match'
    except OSError:
        return False, 'missing required record, answer or live shell'
    except (ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'incomplete or incorrect state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
