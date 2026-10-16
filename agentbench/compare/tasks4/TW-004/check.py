import json, os
from pathlib import Path
EXPECTED = {'catalog': {'events': ['select:archive:1', 'B09916E266', 'save:1', 'select:archive:2', '7CAB2638BC', 'save:2', 'select:archive:3', 'F78A84F3CB', 'save:3', 'select:archive:4', '3BEF8872C1', 'retry:validation', 'save:4', 'select:archive:5', 'BF6F4603BD', 'save:5', 'select:archive:6', '878FB27318', 'save:6', 'select:archive:7', '608E6F9CA6', 'save:7', 'select:archive:8', '48A85E15E4', 'save:8'], 'result': {'catalog': '385EBFF290', 'pages': 8}}}
ANSWER = 'catalog=385EBFF290\npages=8\n'
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
