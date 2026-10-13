import json, sys, os, hashlib
from pathlib import Path
def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    def rec(name):
        value = json.loads((w / '.fx' / (name + '.json')).read_text())
        if name.endswith('.meta'):
            assert value['stdin_tty'] and value['stdout_tty'] and value['tty'], 'Fixture did not use terminals for stdin and stdout'
        return value
    def result(name):
        rec(name + '.meta')
        r = rec(name + '.result')
        assert r['code'] == 0, name + ' did not complete successfully'
        return r
    def answer(expected):
        assert (w / 'answer.txt').read_text() == expected, 'answer.txt differs from the exact expected content'
    def alive(name):
        pid = int((w / '.fx' / (name + '.pid')).read_text())
        os.kill(pid, 0)
        cmd = Path('/proc') / str(pid) / 'cmdline'
        assert b'python3' in cmd.read_bytes(), name + ' no longer runs'
        return pid
    try:
        r=result('board')
        auth=hashlib.sha256(b'beta:production:audited:page2').hexdigest()[:12]
        assert (r['release'],r['environment'],r['audit'],r['authorization'],r['confirmed'])==('beta','production',True,auth,True)
        expected=[repr(k) for k in ['\x1b[B','\r','\x1b[6~','q']]+['environment=production']+[repr(k) for k in [' ','\t','\r','\x1b[C','\r']]+['authorization='+auth]
        assert r['events']==expected, 'Wrong pager/form/modal workflow'
        receipt='beta-production-audit-'+hashlib.sha256(auth.encode()).hexdigest()[:12]
        assert r['receipt']==receipt
        answer(receipt+'\n')
        return True, 'Exact answer and fixture completion records verified'
    except OSError:
        return False, 'Required file or live fixture process is missing'
    except (AssertionError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc) or 'Required final state is missing'
if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    sys.exit(0 if ok else 1)
