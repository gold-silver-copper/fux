import json, hashlib, os
from pathlib import Path
TASK='TW-017'
def code(label,n=0):
    return hashlib.sha256((TASK+':'+label+':'+str(n)).encode()).hexdigest()[:10]
def check(work: str) -> tuple[bool, str]:
    w=Path(work)
    try:
        def record(name):
            r=json.loads((w/'.fx'/(name+'.json')).read_text())
            assert r['tty'] is True and r['ok'] is True and not r['early'], name+' did not complete in a terminal'
            return r
        d=record('desk'); a=record('amber'); b=record('blue')
        assert d['events']==['resolve '+str(n) for n in range(1,9)]
        for role,r in [('amber',a),('blue',b)]:
            ev=[]
            for n in range(1,9):
                if n==4 and role=='blue': ev.append('reset')
                ev.append('armed '+str(n))
            assert r['events']==ev
        assert len({d['ttyname'],a['ttyname'],b['ttyname']})==3
        assert max(r['start'] for r in (d,a,b))<min(r['end'] for r in (d,a,b))
        expected=['round=%02d winner=%s token=%s'%(n,'amber' if n%2 else 'blue',code('amber' if n%2 else 'blue',n)) for n in range(1,9)]
        assert (w/'answer.txt').read_bytes()==('\n'.join(expected)+'\n').encode(), 'Answer does not match the required ledger'
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
