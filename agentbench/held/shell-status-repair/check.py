import hashlib
import json
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    root = Path(work)
    try:
        def seeded(label, modulus, base=0):
            seed = (str(root.resolve()) + ':' + label).encode()
            return base + int.from_bytes(hashlib.sha256(seed).digest()[:8], 'big') % modulus
        INGEST_RC = seeded('ingest-status', 30, 3)

        def rec(name):
            data = json.loads((root / '.fx' / (name + '.json')).read_text())
            if not (data.get('stdin_tty') is True and data.get('stdout_tty') is True):
                raise ValueError('fixture did not run with terminal stdin and stdout')
            return data

        def require(condition, reason):
            if not condition:
                raise ValueError(reason)
        require((root / 'answer.txt').read_bytes().decode('utf-8') == f'ingest={INGEST_RC}\nrepair=0\ntag=tidal batch\ncwd=project\n', 'incorrect shell status report')
        for mode, status, code in [('ingest', 'partial', INGEST_RC), ('repair', 'done', 0)]:
            data = rec('shell_job_' + mode)
            require(data['status'] == status and data['exit_code'] == code and (data['cwd'] == str(root.resolve() / 'project')) and (data['tag'] == 'tidal batch') and data['terminal'], 'wrong shell context or outcome')
        require(rec('shell_job_repair')['prior_status'] == INGEST_RC and rec('shell_job_repair')['actions'] == [f'retry {INGEST_RC}'], 'ingest status was not preserved')
        return (True, 'all required outcomes verified')
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return (False, str(exc))
if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
