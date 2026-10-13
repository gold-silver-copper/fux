import json, sys, os
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        import hashlib
        rows = []
        data = {}
        cache_status = 1 + int(hashlib.sha256(b'cache:status').hexdigest()[:2], 16) % 63
        for job, code in [('index', 0), ('cache', cache_status)]:
            d = json.loads((w / ('.fx/maintenance_' + job + '.json')).read_text())
            data[job] = d
            receipt = hashlib.sha256((job + ':footer').encode()).hexdigest()[:12]
            reply = 'accept ' + hashlib.sha256(b'index:ready').hexdigest()[:12] if job == 'index' else 'ack E-CACHE-' + hashlib.sha256(b'cache:fatal').hexdigest()[:6]
            assert d['stdin_tty'] is True and d['stdout_tty'] is True, 'Fixture did not run in a terminal'
            assert d['starts'] == 1 and d['phase'] == 'done' and d['exit_code'] == code, 'A maintenance outcome is incomplete'
            assert d['inputs'] == [reply] and d['receipt'] == receipt
            rows.append(f'{job} {receipt} {code}\n')
        assert max(d['started_ns'] for d in data.values()) < min(d['reply_ns'] for d in data.values()), 'Jobs did not overlap before replies'
        assert (w / 'answer.txt').read_bytes().decode('utf-8') == ''.join(rows), 'Footer receipts or actual statuses are wrong'
        return True, 'Exact answer and complete fixture protocol verified'
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or 'Required evidence is missing or incorrect'

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
