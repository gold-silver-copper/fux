import json, hashlib, os
from pathlib import Path
TASK='TW-012'
def code(label,n=0):
    return hashlib.sha256((TASK+':'+label+':'+str(n)).encode()).hexdigest()[:10]
def check(work: str) -> tuple[bool, str]:
    w=Path(work)
    try:
        def record(name):
            r=json.loads((w/'.fx'/(name+'.json')).read_text())
            assert r['tty'] is True and r['ok'] is True and not r['early'], name+' did not complete in a terminal'
            return r
        d=record('pack'); a=record('decode'); b=record('sign'); v=record('visitor')
        order=sorted(range(1,9),key=lambda i:code('order',i))
        assert d['events']==['attached '+str(i) for i in order]
        assert a['events']==['decode '+str(i) for i in order]
        assert b['events']==['sign '+str(i) for i in order]
        assert v['events']==['visited']
        assert len({d['ttyname'],a['ttyname'],b['ttyname'],v['ttyname']})==4
        assert max(r['start'] for r in (d,a,b))<v['start']<v['end']<min(r['end'] for r in (d,a,b))
        expected=['bundle=%02d item=%02d seal=%s'%(n,i,code('bundle',i)) for n,i in enumerate(order,1)]
        assert (w/'answer.txt').read_bytes()==('\n'.join(expected)+'\n').encode(), 'Incorrect bundle ledger'
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
