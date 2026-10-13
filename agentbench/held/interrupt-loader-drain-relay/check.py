from pathlib import Path
import json
import hashlib

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        l = json.loads((w / '.fx/loader.json').read_text())
        r = json.loads((w / '.fx/relay.json').read_text())
        assert l['status'] == 'complete' and l['exit_code'] == 130, 'loader did not exit from safe interrupt'
        assert (l['signal'], l['window'], l['interrupts']) == ('SIGINT', 'open', 1), 'wrong loader interrupt'
        assert r['status'] == 'complete' and r['exit_code'] == 0 and r['shutdown'] == 'clean', 'relay not shut down cleanly'
        assert (r['ignored'], r['drained']) == (1, 7) and r['inputs'] == ['drain fb66b1c994'], 'wrong relay drain'
        assert (w / 'answer.txt').read_bytes() == b'loader=130\nrelay=0\nignored=1\ndrained=7\n', 'incorrect signal answer'
        for name in ['loader', 'relay']:
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
