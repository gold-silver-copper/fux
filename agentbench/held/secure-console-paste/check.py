import json
import os
from pathlib import Path
import sys

def check(work: str) -> tuple[bool, str]:
    w = Path(work).resolve()
    def rec(name):
        r = json.loads((w / '.fx' / (name + '.json')).read_text())
        assert r['tty'] and r['stdout_tty'], 'stdin and stdout must both use terminals'
        assert not (w / '.fx' / (name + '.restart')).exists(), 'program was restarted'
        assert 'failure' not in r, r.get('failure')
        assert int((w / '.fx' / (name + '.pid')).read_text()) == r['pid'], 'PID record mismatch'
        return r
    def done(r, code=0):
        assert r.get('finished') is True and r.get('exit_code') == code, 'missing successful completion'
    def answer(expected):
        assert (w / 'answer.txt').read_bytes() == expected.encode('utf-8'), 'answer.txt differs from the exact expected bytes'
    def alive(r):
        os.kill(r['pid'], 0)
        # Detect zombie processes too; this suite runs on Linux.
        assert Path('/proc/' + str(r['pid']) + '/stat').read_text().split(') ', 1)[1][0] != 'Z', 'process is a zombie'
        assert os.readlink('/proc/' + str(r['pid']) + '/fd/0') == r['tty'], 'PID no longer belongs to this terminal'
    try:
        r = rec('secure-console')
        done(r)
        expected = 'Release "copper"\n\nLiteral: $HOME; $(echo nope)\nPath: C:\\drop\\notes\nEND.\n'
        assert r['paste_text'] == expected and r['paste_bytes'] == len(expected.encode())
        assert r['target'] == 'staging' and r['password_ok'] and r['accepted']
        assert [e['name'] for e in r['events']] == ['target', 'alt-v', 'f2', 'password', 'paste', 'ctrl-d', 'exit']
        answer('note=accepted\nbytes=' + str(len(expected.encode())) + '\n')

        return True, 'Exact answer and required fixture evidence verified'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
