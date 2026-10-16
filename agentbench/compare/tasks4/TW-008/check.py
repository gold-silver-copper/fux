import json, os
from pathlib import Path
EXPECTED = {'archive': {'events': ['vault 43BAC0CFD0', 'count 9758', 'bookmark B9EA35D062', 'bookmark E0DBDA05AE', 'bookmark AE462D6259', 'bookmark 34A8CE50FD', 'bookmark 9552BAAC31', 'bookmark 404665E765', 'bookmark BBFEEB0023', 'bookmark AE1750D18A', 'bookmark 4DA3BD0196', 'bookmark 5D71AE381B', 'bookmark 31BFCF3312', 'bookmark 116EE40068', 'bookmark 0351B63824', 'bookmark B3AD251F61', 'bookmark 9B53B032ED', 'bookmark 5916C0B609', 'bookmark 69687C197E', 'bookmark E19EBA6BFF', 'bookmark E41D3BFD11', 'bookmark 65908A75A3'], 'result': {'vault': '43BAC0CFD0', 'count': 9758, 'index': '6330F9DBC8'}}}
ANSWER = 'vault=43BAC0CFD0\ncount=9758\nindex=6330F9DBC8\n'
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
