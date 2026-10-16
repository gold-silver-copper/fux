import json, hashlib, os
from pathlib import Path
TASK='TW-019'
def code(label,n=0):
    return hashlib.sha256((TASK+':'+label+':'+str(n)).encode()).hexdigest()[:10]
def check(work: str) -> tuple[bool, str]:
    w=Path(work)
    try:
        def record(name):
            r=json.loads((w/'.fx'/(name+'.json')).read_text())
            assert r['tty'] is True and r['ok'] is True and not r['early'], name+' did not complete in a terminal'
            return r
        s=record('spool'); c=record('console'); v=record('inspection')
        assert s['events']==['queue '+str(n) for n in range(1,25)]
        assert c['events']==['bind '+str(n) for n in range(10,25)]
        assert v['events']==['inspected']
        cp=json.loads((w/'.fx'/'checkpoint.json').read_text())
        assert cp=={'last':24,'commits':list(range(1,25))}
        fault=json.loads((w/'.fx'/'crash.json').read_text())
        assert fault['tty'] is True and fault['expected_crash'] is True and fault['status']==83 and not fault['early']
        assert fault['events']==['bind '+str(n) for n in range(1,10)]
        assert max(s['start'],fault['start'])<fault['end']<v['start']<v['end']<c['start']<s['end']<c['end']
        assert s['ttyname']!=c['ttyname'] and s['ttyname']!=v['ttyname']
        expected=['job=%02d checkpoint=%s'%(n,code('checkpoint',n)) for n in range(1,25)]
        assert (w/'answer.txt').read_bytes()==('\n'.join(expected)+'\n').encode(), 'Incorrect checkpoint ledger'
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
