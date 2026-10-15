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
        b=rec(work,'builder'); s=rec(work,'server'); p=rec(work,'probe')
        request=code('compile',913); artifact=code('artifact',request,14); session=code('session',artifact,14); token=code('first-failure',session); receipt=code('server-receipt',session+':'+token)
        expected='artifact='+artifact+'\nsession='+session+'\nfirst=FAIL:'+token+'\nreceipt='+receipt+'\n'
        ok=all(good(r) and r.get('normal_exit') is True for r in (b,s,p)) and b.get('inputs')==['compile '+request,'quit'] and s.get('inputs')==['attach '+artifact,'rollback '+token,'quit'] and p.get('inputs')==['probe '+session,'quit'] and b.get('artifact')==artifact and s.get('session')==session and s.get('receipt')==receipt and s.get('settled') is True and p.get('closed') is True and p.get('overlap') is True and len(set(p.get('concurrent_terminals',[])))==3 and p.get('first_kind')=='FAIL' and p.get('first_token')==token and answer(work,expected)
        return bool(ok),'Concurrent build, first test outcome, settlement, and exits verified.' if ok else 'Invalid dependency order, first outcome, concurrency, or answer.'
    except (OSError,ValueError,KeyError,TypeError): return False,'Missing or invalid coordinated-program evidence.'
