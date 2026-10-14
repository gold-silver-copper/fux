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
        rows=[(i*37+11)%1000 for i in range(96)]
        assert (w/'input.csv').read_text()==''.join(f'{v}\n' for v in rows)
        pids=[rec(f'shard{i:02d}.meta')['pid'] for i in range(16)]
        ttys=[rec(f'shard{i:02d}.meta')['tty'] for i in range(16)]
        assert len(set(ttys))==16 and all(ttys)
        tokens=[]
        for n in range(16):
            values=rows[n::16]; tok=hashlib.sha256(','.join(map(str,values)).encode()).hexdigest()[:16]
            expected={'n':n,'count':len(values),'sum':sum(values),'token':tok}
            r=result(f'shard{n:02d}')
            assert r['part']==expected and r['peers']==pids and r['released'] and r['events']==[]
            tokens.append(tok)
        c=result('collector')
        assert rec('collector.meta')['tty'] not in ttys and rec('collector.meta')['tty']
        ticket=hashlib.sha256(('fleet:'+':'.join(tokens)).encode()).hexdigest()[:12]
        assert c['events']==[f'part {n:02d} {tokens[n]}' for n in range(16)]+['publish '+ticket]
        assert rec('fleet-release')=={'parts':list(range(16)),'ticket':ticket}
        summary=f"SUMMARY count=96 sum={sum(rows)} sha256={hashlib.sha256(','.join(map(str,rows)).encode()).hexdigest()}"
        assert c['summary']==summary
        answer(summary+'\n')
        return True, 'Exact answer and fixture completion records verified'
    except OSError:
        return False, 'Required file or live fixture process is missing'
    except (AssertionError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc) or 'Required final state is missing'
if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    sys.exit(0 if ok else 1)
