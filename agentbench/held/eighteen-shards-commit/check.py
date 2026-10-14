import json
import os
from pathlib import Path

def read(work, name):
    record = json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    need(record.get('stdin_tty') is True and record.get('stdout_tty') is True,
         'fixture did not run with terminal stdin and stdout')
    if record.get('status') == 'done':
        command = Path('/proc') / str(record['pid']) / 'cmdline'
        if command.exists():
            data = command.read_bytes()
            need(not (b'python3' in data and str(Path(work).resolve()).encode() in data), 'program has not exited yet')
    return record

def need(condition, reason):
    if not condition:
        raise ValueError(reason)

def events(work):
    return [json.loads(s) for s in (Path(work) / '.fx/events.jsonl').read_text().splitlines()]

def live(record):
    os.kill(record['pid'], 0)
    # A reused PID, dead terminal child, or zombie cannot pass persistence checks.
    cmd = Path('/proc') / str(record['pid']) / 'cmdline'
    need(cmd.exists() and b'python3' in cmd.read_bytes(), 'fixture is no longer running')

def check(work: str) -> tuple[bool, str]:
    try:
        ev = events(work)
        need(not any(e['event'] == 'failed' for e in ev), 'a fixture rejected the protocol')
        from collections import Counter
        starts = Counter(e['program'] for e in ev if e['event'] == 'start')
        need(starts == Counter(['shard-0', 'shard-1', 'shard-2', 'shard-3', 'shard-4', 'shard-5', 'shard-6', 'shard-7', 'shard-8', 'shard-9', 'shard-10', 'shard-11', 'shard-12', 'shard-13', 'shard-14', 'shard-15', 'shard-16', 'shard-17']), 'run each required invocation exactly once')
        return validate(Path(work))
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        return False, str(exc) or type(exc).__name__

def validate(work):
    import hashlib
    shards=[]
    for i in range(18):
        count=120+(i*43)%197
        digest=hashlib.sha256(f'shard-scan|{i}|{count}'.encode()).hexdigest()[:18]
        shards.append(dict(id=i,count=count,digest=digest))
    text=','.join(f'{s["id"]}:{s["count"]}:{s["digest"]}' for s in shards)
    root=hashlib.sha256(text.encode()).hexdigest()
    for s in shards:
        i=s['id']; rec=read(work,f'shard-{i}')
        need(rec['status']=='done' and rec['exit_code']==0 and rec['root']==root, f'shard {i} not committed')
        need(rec['count']==s['count'] and rec['digest']==s['digest'], f'shard {i} result differs')
        nonce=hashlib.sha256(f'dispatch|shard-{i}|v4'.encode()).hexdigest()[:12]
        need(rec['inputs']==['scan '+nonce,'commit '+root], f'shard {i} used wrong nonce or root')
    ev=events(work)
    need(max(k for k,e in enumerate(ev) if e['event']=='start')<min(k for k,e in enumerate(ev) if e['event']=='dispatched'), 'dispatch before every shard started')
    need(max(k for k,e in enumerate(ev) if e['event']=='result')<min(k for k,e in enumerate(ev) if e['event']=='committed'), 'commit before every result')
    answer=(work/'answer.txt').read_text()
    need(answer.endswith('\n') and len(answer.splitlines())==1, 'answer must be one newline-terminated JSON line')
    actual=json.loads(answer)
    need(actual==dict(shards=shards,total=sum(s['count'] for s in shards),root=root), 'unordered, incomplete or incorrect reduction')
    need(type(actual['total']) is int and all(type(s['id']) is int and type(s['count']) is int for s in actual['shards']), 'IDs and counts must be integers')
    return True, 'all eighteen dispatches, results and aggregate commits verified'

if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
