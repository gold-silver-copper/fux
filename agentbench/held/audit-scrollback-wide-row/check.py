from pathlib import Path
import json
import hashlib

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        r = json.loads((w / '.fx/audit_view.json').read_text())
        assert r['status'] == 'complete' and r['inputs'] == ['freeze'], 'snapshot not frozen'
        assert (r['archive'], r['wide'], r['queue']) == ('11290c3e16b00f57', '76cb2f29f5a5c649da', 1017), 'wrong snapshot record'
        assert (w / 'answer.txt').read_bytes() == b'archive=11290c3e16b00f57\nwide=76cb2f29f5a5c649da\nqueue=1017\n', 'incorrect screen values'
        for name in ['audit_view']:
            data = json.loads((w / '.fx' / (name + '.json')).read_text())
            assert data.get('stdin_tty') is True and data.get('stdout_tty') is True, 'fixture did not run in a terminal'
        return True, "exact answer and completed protocol verified"
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, "missing or incorrect final state: " + str(exc)

if __name__ == '__main__':
    import sys
    passed, reason = check(sys.argv[1])
    print(json.dumps({'passed': passed, 'reason': reason}))
    raise SystemExit(0 if passed else 1)
