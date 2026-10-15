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
        r=rec(work,'archive'); a=code('archive',452); tail=code('tail',a,16); receipt=code('sealed',tail)
        expected='archive='+a+'\ntail='+tail+'\nreceipt='+receipt+'\n'
        inputs=['stream '+code('stream',a),'continue '+code('continue',a),'seal '+a+' '+tail]
        ok=good(r) and r.get('normal_exit') is True and r.get('initial_size')==[72,18] and r.get('resumed_size')==[113,31] and r.get('scan_bytes',0)>1048576 and r.get('inputs')==inputs and r.get('archive')==a and r.get('tail')==tail and r.get('receipt')==receipt and answer(work,expected)
        return bool(ok), 'Archive, wrapped tail, and resize verified.' if ok else 'Incorrect archive/tail, missing resize, or invalid terminal run.'
    except (OSError,ValueError,KeyError,TypeError): return False,'Missing or invalid archive evidence.'
