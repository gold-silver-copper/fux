import json, os
from pathlib import Path
EXPECTED = {'patchdesk': {'events': ['view:1', 'BEGIN 1', 'seal 75A58D56F1', 'END', 'commit:1', 'view:2', 'BEGIN 2', 'seal 8AFAB90095', 'END', 'commit:2', 'view:3', 'BEGIN 3', 'seal 5E02A159C4', 'END', 'commit:3', 'view:4', 'BEGIN 4', 'seal F83B841733', 'END', 'commit:4', 'view:5', 'BEGIN 5', 'seal B690928264', 'END', 'commit:5', 'view:6', 'BEGIN 6', 'seal C4EAFA6891', 'END', 'commit:6', 'view:7', 'BEGIN 7', 'seal 4169032F73', 'END', 'commit:7'], 'result': {'patchset': '4F5B7130D8', 'commits': 7}}}
ANSWER = 'patchset=4F5B7130D8\ncommits=7\n'
def check(work: str) -> tuple[bool, str]:
    root=Path(work)
    try:
        if (root/'answer.txt').read_bytes()!=ANSWER.encode():
            return False, 'answer.txt does not have the exact required lines'
        for name,expected in EXPECTED.items():
            p=root/'.fx'/(name+'.json')
            r=json.loads(p.read_text())
            if r.get('tty') is not True or r.get('invalid') is not False or r.get('success') is not True:
                return False, name+': missing successful terminal run or input arrived while busy'
            try:
                os.kill(r['pid'], 0)
            except ProcessLookupError:
                pass
            else:
                return False, name+': program has not exited'
            if name=='command':
                try:
                    os.kill(r['shell_pid'], 0)
                except ProcessLookupError:
                    pass
                else:
                    return False, 'interactive shell has not exited'
            if r.get('events') != expected['events'] or r.get('result') != expected['result']:
                return False, name+': incomplete or incorrect interaction transcript'
        return True, 'all terminal handshakes and exact answer verified'
    except (OSError,ValueError,TypeError,KeyError) as e:
        return False, 'missing or malformed evidence: '+str(e)
