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
        if c != {'seed': 'lichen-92', 'destination': 'harbor-east', 'note': 'Hold for green manifest', 'mode': 'verified'}:
            return False, 'job.json changed from setup'
        r = records(work, 'dispatch_form')
        batch = code('batch', c['seed'], c['destination'], c['note'], c['mode'])
        challenge = code('form-challenge', batch)
        receipt = code('form-receipt', batch, challenge)
        ack = 'commit ' + challenge + ' receipt=' + code('preview', c['seed'])
        if r.get('form') != [c['destination'], c['note'], c['mode']] or r.get('inputs') != [ack]:
            return False, 'form or acknowledgement differs'
        if r.get('closed') is not True or r.get('batch') != batch or r.get('receipt') != receipt:
            return False, 'form was not committed and closed'
        expected = 'batch=' + batch + '\nreceipt=' + receipt + '\n'
        if (work / 'answer.txt').read_bytes() != expected.encode():
            return False, 'answer.txt differs from the exact expected lines'
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc)
