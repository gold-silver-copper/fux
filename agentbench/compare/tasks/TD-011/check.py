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
        r=rec(work,'audit'); token=code('manual',731); receipt=code('audit-receipt',token)
        expected='first=MANUAL:'+token+'\nreceipt='+receipt+'\n'
        ok=good(r) and r.get('normal_exit') is True and r.get('first_kind')=='MANUAL' and r.get('first_token')==token and r.get('receipt')==receipt and r.get('inputs',[])[0]=='begin '+code('audit-start',731) and r['inputs'][-1]=='choose MANUAL '+token and all(x=='replay' for x in r['inputs'][1:-1]) and answer(work,expected)
        return bool(ok), 'First current event and receipt match.' if ok else 'Need the first current event, valid terminal audit, and exact answer.'
    except (OSError,ValueError,KeyError,IndexError,TypeError): return False,'Missing or invalid audit evidence.'
