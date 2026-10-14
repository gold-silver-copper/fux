
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
        rows = []
        assert len({read(f'worker.{i}.terminal.json')['stdin'] for i in range(8)}) == 8, 'eight distinct terminals required'
        for i in range(8):
            ran(f'worker.{i}')
            retries = int(i in (2, 5))
            command = 'commit ' + digest('commit:' + str(i), 10)
            result = 'W-' + digest(f'worker:{i}|retry:{retries}|done')
            assert read(f'worker.{i}.authorized.json') == {'command': command}
            assert read(f'worker.{i}.json') == {'index': i, 'retries': retries, 'commit': command, 'cohort': 8, 'result': result}
            rows.append(f'{i} {result}\n')
        assert (w / 'answer.txt').read_text() == ''.join(rows)

        return True, 'exact answer and fixture records match'
    except OSError:
        return False, 'missing required record, answer or live shell'
    except (ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'incomplete or incorrect state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
