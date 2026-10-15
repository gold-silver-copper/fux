import json
import hashlib
from pathlib import Path

def code(label, *parts):
    return hashlib.sha256('|'.join([label] + [str(p) for p in parts]).encode()).hexdigest()[:10].upper()

def records(work, name):
    paths = list((work / '.fx').glob(name + '.run-*.json'))
    if len(paths) != 1:
        raise ValueError(name + ': expected exactly one launch')
    record = json.loads(paths[0].read_text())
    if record.get('tty') is not True or record.get('success') is not True or record.get('state') != 'finished':
        raise ValueError(name + ': no successful terminal run')
    return record

def check(work: str) -> tuple[bool, str]:
    try:
        work = Path(work)
        c = json.loads((work / 'job.json').read_text())
        if c != {'seed': 'copper-61', 'target': 'image-arm64'}:
            return False, 'job.json changed from setup'
        b = records(work, 'release_build')
        a = records(work, 'release_attest')
        status = 17 + int(code('exit', c['seed']), 16) % 49
        ticket = code('inspect', c['seed'], c['target'])
        seal = code('seal', c['seed'], c['target'], status)
        if 'i' not in b.get('shell_flags', '') or 'i' not in a.get('shell_flags', ''):
            return False, 'interactive Bash evidence missing'
        if b.get('requested_exit') != status or a.get('observed_status') != str(status) or a.get('seal') != seal:
            return False, 'shell exit status was not attested'
        if b.get('inputs') != ['inspect target=' + c['target'] + ' ticket=' + ticket + ' status=0']:
            return False, 'inspection input differs'
        if a.get('inputs') != ['seal target=' + c['target'] + ' ticket=' + code('seal-ticket', ticket)]:
            return False, 'seal input differs'
        expected = 'status=' + str(status) + '\nseal=' + seal + '\n'
        if (work / 'answer.txt').read_bytes() != expected.encode():
            return False, 'answer.txt differs from the exact expected lines'
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc)
