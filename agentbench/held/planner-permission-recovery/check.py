
import hashlib
import json
from pathlib import Path
import sys

def check(work: str) -> tuple[bool, str]:
    root = Path(work)
    try:
        def record(name):
            value = json.loads((root / '.fx' / (name + '.json')).read_text())
            assert value['terminal'] == {'stdin': True, 'stdout': True}, name + ' did not run in a terminal'
            assert value['seed'] == hashlib.sha256(('TA-030:' + str(root.resolve())).encode()).hexdigest()
            assert value['done'] is True, name + ' did not finish'
            assert value['error'] is None, name + ' failed its protocol'
            return value
        def answer(expected):
            assert (root / 'answer.txt').read_bytes() == expected.encode(), 'answer.txt differs'
        def inputs(value):
            return [e['value'] for e in value['events'] if e['kind'] == 'input']
        def kinds(value):
            return [e['kind'] for e in value['events']]
        import hashlib
        value = record('planner-agent')
        version = int((root / 'manifest.txt').read_text().splitlines()[0].split('=',1)[1])
        result = 'manifest-%d:fixed' % version
        digest = hashlib.sha256(result.encode()).hexdigest()[:12]
        assert value['exit'] == 0
        assert inputs(value) == ['approve workspace','allow manifest.txt','deny delete','inspect lock','retry serial','accept '+digest]
        assert kinds(value) == ['started','input','input','manifest_read','input','input','input','result','input','accepted']
        assert (root / 'manifest.txt').read_bytes() == b'manifest-version=17\nstate=needs-fix\n'
        assert value['events'][-1] == {'kind':'accepted','result':result}
        answer('plan=workspace\npermission=manifest-only\nrecovery=serial\nresult=%s\n' % result)
        return True, 'exact result and fixture protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or 'invalid final state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
