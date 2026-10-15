import json, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        seed = 'preview/handoff/308'
        assert (w / 'seed.txt').read_text() == 'preview/handoff/308\n', 'setup seed changed'
        def token(tag): return hashlib.sha256((seed + '|' + tag).encode()).hexdigest()[:12].upper()
        def record(name):
            r = json.loads((w / '.fx' / (name + '.success.json')).read_text())
            assert r.get('terminal') is True and isinstance(r.get('pid'), int) and r.get('tty'), 'terminal evidence missing'
            return r
        c, p, s = record('compile'), record('probe'), record('serve')
        assert c['command'] == 'build READY'
        assert c['probe_pid'] == p['pid'] and c['serve_pid'] == s['pid']
        assert p['compiler_pid'] == c['pid'] and p['serve_pid'] == s['pid']
        assert s['compiler_pid'] == c['pid'] and s['probe_pid'] == p['pid']
        assert len(c['ttys']) == 3 and len(set(c['ttys'])) == 3
        assert c['ttys'] == [c['tty'], p['tty'], s['tty']]
        assert p['test'] == 'test ' + token('artifact') and p['finish'] == 'finish'
        assert s['publish'] == 'publish ' + token('verdict') and s['stop'] == 'stop ' + token('deployment')
        assert c['artifact'] == token('artifact') and p['verdict'] == token('verdict') and s['deployment'] == token('deployment')
        assert (w / 'answer.txt').read_bytes() == ('artifact=' + token('artifact') + '\nseal=' + token('verdict') + '\ndeployment=' + token('deployment') + '\n').encode(), 'incorrect handoff values' 
        return True, 'exact answer and terminal protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return False, str(e) or 'final state does not match'
