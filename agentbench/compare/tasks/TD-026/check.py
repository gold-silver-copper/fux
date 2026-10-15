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
        r = record(work, 'mirror')
        values = [route + '/' + token('mirror-first-' + str(i), 14) for i, route in ((1, 'fallback'), (2, 'primary'))]
        expected_inputs = []
        for i, value in enumerate(values, 1):
            expected_inputs += ['arm ' + token('mirror-arm-' + str(i)), 'retain ' + value.replace('/', ' ')]
        if r.get('results') != values or r.get('inputs') != expected_inputs:
            raise ValueError('the first routes were not retained')
        answer(work, ''.join('race' + str(i) + '=' + v + '\n' for i, v in enumerate(values, 1)))
        return True, 'both first routes retained'
    except (OSError, ValueError, KeyError, TypeError) as e:
        return False, str(e)
