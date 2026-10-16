import json, os
from pathlib import Path
EXPECTED = {'assistant': {'events': ['grant AF0DF6D8A8', 'grant 953C941D84', 'grant 41026B36E3', 'grant 667EF832D2', 'grant C989F3E107', 'grant 2A27D14408', 'grant 6C93F7BEC0', 'permission:local', 'grant 7A80A43192', 'grant 0593424F35', 'grant 11F665C926', 'grant 3BD06EF24C', 'grant 75F671EE24', 'grant 55C886D3C0', 'grant CB0A1829C6', 'cancel:tool', 'grant 5B48CD106E', 'grant 8635A93E77', 'grant F5D18A33E3', 'grant 770AE28914', 'grant 0B2455EB00', 'grant C849730903', 'grant 05291FFE37', 'grant 382B229BEB'], 'result': {'revision': 'F46248A6D1', 'scope': 'local-only'}}}
ANSWER = 'revision=F46248A6D1\nscope=local-only\n'
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
