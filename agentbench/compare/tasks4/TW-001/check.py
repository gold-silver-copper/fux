import json, os
from pathlib import Path
EXPECTED = {'rebuild': {'events': ['approve 6C14071017', 'approve EC8FC30C30', 'approve 52241690FD', 'approve 7A4AE7A805'], 'result': {'artifact': '24AB7D4AC7'}}, 'server': {'events': ['load 24AB7D4AC7', 'deploy 83336FFEE8', 'deploy ED9C1FB427', 'deploy 7F1B2656BC', 'deploy 656FF09080', 'deploy 9D10F9F89D', 'deploy 9D274DFE54'], 'result': {'endpoint': '7BBC24329C'}}, 'canary': {'events': ['target 7BBC24329C', 'verify 4CACAF6612', 'verify F17615C9F1', 'verify A679293AB5', 'verify F58D5958E0', 'verify 2AF8A069CE', 'verify 3D25B40B0D'], 'result': {'release': '2D6D98A848'}}, 'build': {'events': ['approve 88690BE4C0', 'approve F7C6EF4132', 'approve C12611E0A6', 'approve 9BD8BC8DED', 'approve 5FFBFA47BB', 'approve 2F1D6C55BE', 'approve F9C00B026D', 'approve F03C8EE495'], 'result': {}}}
ANSWER = 'artifact=24AB7D4AC7\nendpoint=7BBC24329C\nrelease=2D6D98A848\n'
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
