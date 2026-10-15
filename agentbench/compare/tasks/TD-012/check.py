import json, hashlib
from pathlib import Path

def code(label, seed, size=10):
    return hashlib.sha256((label + ':' + str(seed)).encode()).hexdigest()[:size].upper()

def rec(work, name):
    r = json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    if not isinstance(r, dict): raise ValueError('record must be an object')
    return r

def good(r):
    return r.get('runs') == 1 and r.get('tty') is True and r.get('success') is True and r.get('premature') == []

def answer(work, expected):
    return (Path(work) / 'answer.txt').read_bytes() == expected.encode('utf-8')

def check(work: str) -> tuple[bool, str]:
    try:
        r=rec(work,'watcher'); ticket=code('ticket',194); token=code('review',ticket); receipt=code('settled',token)
        expected='ticket='+ticket+'\nfirst=REVIEW:'+token+'\nreceipt='+receipt+'\n'
        inputs=['watch '+code('watch',194)+' label=COMPLETE:FAULT','settle REVIEW '+token]
        ok=good(r) and r.get('normal_exit') is True and r.get('inputs')==inputs and r.get('ticket')==ticket and r.get('first_token')==token and r.get('receipt')==receipt and answer(work,expected)
        return bool(ok), 'Current first outcome settled after the window opened.' if ok else 'Wrong first outcome, early input, or incorrect answer.'
    except (OSError,ValueError,KeyError,TypeError): return False,'Missing or invalid watcher evidence.'
