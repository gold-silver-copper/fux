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
        ledger=(Path(work)/'ledger.txt').read_text().strip()
        if ledger!='ledger revision violet / tranche 29': return False,'The prepared ledger changed.'
        status=17+int(code('ledger-exit',ledger,4),16)%47
        token=code('shell-receipt',str(status)+':'+ledger)
        job=rec(work,'ledger'); report=rec(work,'shell-receipt')
        expected='status='+str(status)+'\nreceipt='+token+'\n'
        ok=good(job) and good(report) and job.get('normal_exit') is True and report.get('normal_exit') is True and job.get('inputs')==[] and report.get('inputs')==[] and job.get('expected_status')==status and report.get('reported_status')==status and 'i' in report.get('shell_flags','') and report.get('receipt')==token and answer(work,expected)
        return bool(ok), 'Interactive shell exit status and receipt verified.' if ok else 'Need the actual foreground exit status from an interactive terminal shell.'
    except (OSError,ValueError,KeyError,TypeError): return False,'Missing or invalid shell evidence.'
