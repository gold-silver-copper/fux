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
        if c != {'seed': 'alder-73', 'depot': 'coast-west', 'parcel': 'crate-508'}:
            return False, 'job.json changed from setup'
        r = records(work, 'line_dispatch')
        route = code('route', c['seed'], c['depot'])
        challenge = code('challenge', c['seed'], c['parcel'])
        receipt = code('dispatch', route, challenge, c['parcel'])
        inputs = ['ship ' + c['depot'] + ' ' + c['parcel'] + ' route=' + route, 'confirm receipt=' + challenge]
        if r.get('inputs') != inputs or r.get('route') != route or r.get('receipt') != receipt:
            return False, 'dispatch evidence differs'
        expected = 'route=' + route + '\nreceipt=' + receipt + '\n'
        if (work / 'answer.txt').read_bytes() != expected.encode():
            return False, 'answer.txt differs from the exact expected lines'
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc)
