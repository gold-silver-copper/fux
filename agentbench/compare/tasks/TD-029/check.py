import hashlib
import json
from pathlib import Path

def token(label, size=10):
    return hashlib.sha256(('terminal-lab-6107:' + label).encode()).hexdigest()[:size]

def record(work, name):
    r = json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    if r.get('runs') != 1 or r.get('tty') is not True or r.get('early_input'):
        raise ValueError(name + ': not one valid terminal run')
    if r.get('state') != 'done':
        raise ValueError(name + ': not completed')
    if (Path(work) / '.fx' / (name + '.pid')).read_text() != str(r['pid']) + '\n':
        raise ValueError(name + ': missing pid record')
    return r

def answer(work, expected):
    if (Path(work) / 'answer.txt').read_bytes() != expected.encode():
        raise ValueError('answer.txt differs from the exact required lines')

def check(work: str) -> tuple[bool, str]:
    try:
        d, a = record(work, 'dispatch'), record(work, 'audit')
        status = 17 + int(token('dispatch-status', 4), 16) % 90
        receipt = token('dispatch-first-receipt', 22)
        checkpoint = token('dispatch-audit:' + str(status) + ':' + receipt, 18)
        if d.get('exit_status') != status or a.get('reported_status') != status or d.get('receipt') != receipt or a.get('checkpoint') != checkpoint:
            raise ValueError('dispatch status or audit differs')
        if d.get('inputs') != ['finish RECEIPT ' + token('dispatch-challenge', 14)] or a.get('inputs') != []:
            raise ValueError('wrong terminal interaction')
        if d.get('shell_pid') != a.get('shell_pid') or not d.get('shell_pid') or any('i' not in r.get('shell_flags', '') or r.get('cwd') != str(Path(work).resolve()) for r in (d, a)):
            raise ValueError('commands did not share the interactive shell in work')
        answer(work, 'status=' + str(status) + '\nreceipt=' + receipt + '\ncheckpoint=' + checkpoint + '\n')
        return True, 'delayed shell status and first receipt are correct'
    except (OSError, ValueError, KeyError, TypeError) as e:
        return False, str(e)
