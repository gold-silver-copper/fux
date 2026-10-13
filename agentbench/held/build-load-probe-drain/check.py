import json, sys, os
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        import hashlib
        t = lambda s: hashlib.sha256(s.encode()).hexdigest()[:12]
        assert (w / 'source.cfg').read_text() == 'target=bronze\noptimization=checked\n', 'Source configuration changed'
        source = (w / 'source.cfg').read_text()
        artifact = t('artifact:' + source)
        ready = t('service:' + artifact)
        receipt = t('probe:' + ready)
        b, s, p = [json.loads((w / ('.fx/' + name + '.json')).read_text()) for name in ['builder', 'service', 'probe']]
        for d in [b, s, p]:
            assert d['stdin_tty'] is True and d['stdout_tty'] is True, 'Fixture did not run in a terminal'
            assert d['starts'] == 1 and d['phase'] == 'done' and d['exit_code'] == 0, 'A pipeline program is incomplete'
        assert b['inputs'] == ['build ' + t('build:' + source)] and b['artifact'] == artifact
        assert s['inputs'] == ['load ' + artifact, 'stop ' + receipt] and s['stopped'] is True
        assert p['inputs'] == ['check archive/' + artifact[:6]] and p['receipt'] == receipt
        assert s['waiting_ns'] < b['started_ns'] < b['finished_ns'] < s['ready_ns'] < p['started_ns'] < p['finished_ns'] < s['stop_ns'], 'Wrong pipeline ordering'
        assert (w / 'answer.txt').read_bytes().decode('utf-8') == f'ARTIFACT={artifact}\nPROBE={receipt}\nSERVICE=stopped\n', 'Wrong pipeline summary'
        return True, 'Exact answer and complete fixture protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or 'Required evidence is missing or incorrect'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
