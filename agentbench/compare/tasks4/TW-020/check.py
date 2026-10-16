import json, hashlib, os
from pathlib import Path
TASK='TW-020'
def code(label,n=0):
    return hashlib.sha256((TASK+':'+label+':'+str(n)).encode()).hexdigest()[:10]
def check(work: str) -> tuple[bool, str]:
    w=Path(work)
    try:
        def record(name):
            r=json.loads((w/'.fx'/(name+'.json')).read_text())
            assert r['tty'] is True and r['ok'] is True and not r['early'], name+' did not complete in a terminal'
            return r
        rs=[record('worker%02d'%i) for i in range(8)]
        for r in rs:
            assert r['events']==['lap 1','lap 2','lap 3','sealed'] and r['lap']==3
        assert len({r['ttyname'] for r in rs})==8
        assert max(r['start'] for r in rs)<min(r['end'] for r in rs)
        expected=['worker=%02d result=%s'%(i,code('result',i*10+3)) for i in range(8)]
        assert (w/'answer.txt').read_bytes()==('\n'.join(expected)+'\n').encode(), 'Answer does not match the required ledger'
        v=record('visitor')
        assert v['events']==['visited']
        assert all(r['start']<v['start']<v['end']<r['end'] and r['ttyname']!=v['ttyname'] for r in rs)
        # PID records are immutable per attempt. Earlier failed attempts are
        # allowed, but no fixture from any attempt may remain alive.
        for pidfile in (w/'.fx').glob('*.pid'):
            pid=json.loads(pidfile.read_text())
            assert isinstance(pid,int) and pid>0, 'Invalid fixture PID'
            try:
                os.kill(pid,0)
            except ProcessLookupError:
                continue
            except PermissionError:
                pass
            return False, 'A fixture process is still running'
        return True, 'Exact answer and terminal protocol records verified.'
    except (OSError, ValueError, KeyError, AssertionError, TypeError) as e:
        return False, str(e) or ('Mismatch at check line '+str(e.__traceback__.tb_lineno))
