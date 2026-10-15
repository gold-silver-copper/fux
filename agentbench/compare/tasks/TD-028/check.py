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
        r = record(work, 'ledger')
        header, tail = token('ledger-live-header', 20), token('ledger-live-tail', 24)
        receipt = token('ledger-resize:' + header + ':' + tail + ':132x32', 18)
        if r.get('initial_size') != [72, 24] or r.get('resized_size') != [132, 32] or r.get('early_resize') or r.get('resize_after_live') is not True:
            raise ValueError('resize did not happen at the live barrier')
        if r.get('bulk_bytes') != 40 * 64 * (10 + 512 + 1):
            raise ValueError('bulk ledger was not completed')
        if r.get('inputs') != ['scan ' + token('ledger-scan', 12), 'commit ' + header + ' ' + tail + ' ' + receipt]:
            raise ValueError('live ledger was not committed')
        if [r.get('header'), r.get('tail'), r.get('receipt')] != [header, tail, receipt]:
            raise ValueError('wrong live ledger receipt')
        answer(work, 'header=' + header + '\ntail=' + tail + '\nresize=' + receipt + '\n')
        return True, 'large ledger and resize receipt are correct'
    except (OSError, ValueError, KeyError, TypeError) as e:
        return False, str(e)
