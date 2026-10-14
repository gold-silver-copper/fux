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
        rec('nightwatch.meta'); rec('canary.meta')
        n=alive('nightwatch'); c=alive('canary')
        assert n!=c and rec('nightwatch.meta')['tty']!=rec('canary.meta')['tty']
        checkpoint=rec('nightwatch.checkpoint'); state=rec('nightwatch.state')
        assert checkpoint['pid']==state['pid']==n and checkpoint['tick']==8
        ticket=hashlib.sha256(b'journal:nightwatch:tick8').hexdigest()[:12]
        assert state['name']=='archive-nightwatch' and state['events']==['checkpoint '+ticket,'rename archive-nightwatch']
        assert rec('canary.state')=={'name':'canary','pid':c}
        assert rec('nightwatch.heartbeat')['tick']>=8 and rec('canary.heartbeat')['tick']>=8
        assert rec('nightwatch.heartbeat')['pid']==n and rec('canary.heartbeat')['pid']==c
        receipt='journal-'+hashlib.sha256(('checkpoint:'+ticket).encode()).hexdigest()[:12]
        assert checkpoint['receipt']==receipt
        answer('archive-nightwatch='+receipt+'\ncanary=running\n')
        return True, 'Exact answer and fixture completion records verified'
    except OSError:
        return False, 'Required file or live fixture process is missing'
    except (AssertionError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc) or 'Required final state is missing'
if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    sys.exit(0 if ok else 1)
