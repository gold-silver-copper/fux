
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
            assert value['seed'] == hashlib.sha256(('TA-023:' + str(root.resolve())).encode()).hexdigest()
            assert value['done'] is True, name + ' did not finish'
            assert value['error'] is None, name + ' failed its protocol'
            return value
        def answer(expected):
            assert (root / 'answer.txt').read_bytes() == expected.encode(), 'answer.txt differs'
        def inputs(value):
            return [e['value'] for e in value['events'] if e['kind'] == 'input']
        def kinds(value):
            return [e['kind'] for e in value['events']]
        seed = hashlib.sha256(('TA-023:' + str(root.resolve())).encode()).hexdigest()
        anchor = 'R47|warehouse=North Annex|digest=' + ':'.join(seed[i:i+4] for i in range(0,16,4)) + '|owner=Inez Park|retain=2032-06'
        footer = str(5000 + int(seed[16:20],16) % 4000)
        tails = ['%s:%d' % (color, 10 + int(seed[i:i+2],16)) for color,i in [('amber',20),('cobalt',22),('ivory',24)]]
        value = record('ledger')
        assert value['exit'] == 0
        assert inputs(value) == ['show tail', 'accept ' + footer]
        assert kinds(value) == ['started', 'archive_complete', 'input', 'input', 'receipt']
        answer(anchor + '\n' + footer + '\n' + ','.join(tails) + '\n')
        return True, 'exact result and fixture protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or 'invalid final state'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
