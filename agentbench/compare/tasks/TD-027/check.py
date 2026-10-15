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
        r = record(work, 'shipping')
        values = [d + '/' + token('shipping-first-parcel' + str(i), 16) for i, d in ((1, 'REVIEW'), (2, 'CANCEL'))]
        manifest = token('shipping-manifest:' + ':'.join(values), 18)
        inputs = []
        for i, value in enumerate(values, 1):
            p = 'parcel' + str(i)
            inputs += ['DECISION ' + p + ' disposition=RELEASE seal=' + token('shipping-bid-' + p, 12),
                       'settle ' + p + ' ' + value.replace('/', ' ')]
        if r.get('inputs') != inputs or r.get('settlements') != values or r.get('manifest') != manifest:
            raise ValueError('settlements do not reflect the first live decisions')
        answer(work, ''.join('parcel' + str(i) + '=' + v + '\n' for i, v in enumerate(values, 1)) + 'manifest=' + manifest + '\n')
        return True, 'first decisions and manifest are correct'
    except (OSError, ValueError, KeyError, TypeError) as e:
        return False, str(e)
