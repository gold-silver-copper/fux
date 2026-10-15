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
        if c != {'seed': 'quartz-29', 'document': 'inventory-august'}:
            return False, 'job.json changed from setup'
        r = records(work, 'revision_console')
        revision = code('revision', c['seed'], c['document'])
        receipt = code('revision-receipt', revision, c['document'])
        target = 'apply revision=' + revision + ' receipt=' + code('echo', c['seed'])
        if r.get('inputs') != [target] or r.get('revision') != revision or r.get('receipt') != receipt:
            return False, 'canonical input evidence differs'
        expected = 'revision=' + revision + '\nreceipt=' + receipt + '\n'
        if (work / 'answer.txt').read_bytes() != expected.encode():
            return False, 'answer.txt differs from the exact expected lines'
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc)
