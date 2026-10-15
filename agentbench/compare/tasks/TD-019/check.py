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
        if c != {'seed': 'birch-48', 'scope': 'reports/quarter-3'}:
            return False, 'job.json changed from setup'
        r = records(work, 'archive_agent')
        plan = code('plan', c['seed'], c['scope'])
        echo = code('echo-result', c['seed'])
        permission = code('permission', plan, c['scope'])
        result = code('archive-result', plan, permission)
        draft = 'approve plan=' + code('draft-plan', c['seed']) + ' result=' + echo
        inputs = ['plan scope=' + c['scope'], 'approve plan=' + plan + ' result=' + echo,
                  'allow once ' + code('permission-challenge', plan)]
        if r.get('staged_draft') != draft or r.get('inputs') != inputs:
            return False, 'agent staging, approval or permission differs'
        if [r.get('plan'), r.get('permission'), r.get('result')] != [plan, permission, result]:
            return False, 'agent completion evidence differs'
        expected = 'plan=' + plan + '\npermission=' + permission + '\nresult=' + result + '\n'
        if (work / 'answer.txt').read_bytes() != expected.encode():
            return False, 'answer.txt differs from the exact expected lines'
        return True, 'terminal interaction and exact answer verified'
    except (OSError, ValueError, KeyError, TypeError) as exc:
        return False, str(exc)
