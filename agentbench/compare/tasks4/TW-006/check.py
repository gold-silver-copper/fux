import json, os
from pathlib import Path
EXPECTED = {'journal': {'events': ['checkpoint 51D3858D4A', 'checkpoint EF3BF636DB', 'checkpoint ACAB2DC7CD', 'checkpoint D2DDDE6483', 'checkpoint 006E949129', 'checkpoint A82182EB85', 'shutdown C28F1742DA'], 'result': {'journal': 'B8DE7BB487'}}, 'command': {'events': ['interactive', 'apply 5FA7D3F4DD', 'apply 18E82679A3', 'apply A0BC039930', 'apply 96D24D5A80', 'apply 78D890E1C1', 'apply B862FE4AAB', 'apply 09694A6DEB', 'apply C551D8C2C4', 'apply 5DEDF83C78', 'apply D2485661EC'], 'result': {'status': 78, 'migration': '764D411291'}}, 'audit': {'events': ['status 78', 'audit EF9E610974', 'audit 4525272B95', 'audit 4EED0EB1D9', 'audit 8B46C5F799', 'audit B935B6916F', 'audit F50E3CC8DC'], 'result': {'audit': '2D83CCEE38'}}}
ANSWER = 'status=78\nmigration=764D411291\naudit=2D83CCEE38\njournal=B8DE7BB487\n'
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
