
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
            assert value['seed'] == hashlib.sha256(('TA-021:' + str(root.resolve())).encode()).hexdigest()
            assert value['done'] is True, name + ' did not finish'
            assert value['error'] is None, name + ' failed its protocol'
            return value
        def answer(expected):
            assert (root / 'answer.txt').read_bytes() == expected.encode(), 'answer.txt differs'
        def inputs(value):
            return [e['value'] for e in value['events'] if e['kind'] == 'input']
        def kinds(value):
            return [e['kind'] for e in value['events']]
        seed = hashlib.sha256(('TA-021:' + str(root.resolve())).encode()).hexdigest()
        probe_status = 20 + int(seed[:4], 16) % 60
        seal = 'AMBER-96x28-' + seed[4:16].upper()
        probe = record('calibrate-probe')
        recovery = record('calibrate-recover')
        assert probe['exit'] == probe_status and recovery['exit'] == 0
        for value in (probe, recovery):
            assert value['configuration'] == {'cwd': str(root.resolve() / 'station'), 'profile': 'amber', 'cols': 96, 'rows': 28}
        assert kinds(probe) == ['started', 'startup_crash']
        assert inputs(recovery) == ['recover %d' % probe_status, seal]
        assert kinds(recovery) == ['started', 'input', 'input', 'calibrated']
        answer('probe=%d\nrecover=%d\nseal=%s\n' % (probe_status, recovery['exit'], seal))
        return True, 'exact result and fixture protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or 'invalid final state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
