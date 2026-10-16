import json, os
from pathlib import Path
EXPECTED = {'worker0': {'events': ['commit D7EE93BA33', 'previous 78E9B43A66', 'commit 1C0C75FE5E', 'previous 3C8BA6B54E', 'commit 577B20105D'], 'result': {'result': '01B476601E'}}, 'worker1': {'events': ['commit B2865E0BE1', 'previous C1A609FAB5', 'commit 20FEB72F6F', 'previous 6087469B1C', 'commit 24E6E0F055'], 'result': {'result': '973E9AAAB2'}}, 'worker2': {'events': ['commit 386CD332EB', 'previous 1CD83CDE8A', 'commit 9D242DAEC4', 'previous 3857F8E797', 'commit 32CD241DED'], 'result': {'result': '2242854551'}}, 'worker3': {'events': ['commit CC83CFD6F8', 'previous 5A54CFEEAE', 'commit CB2D0EE5E9', 'previous E37E9FCB7D', 'commit FEC4F772E3'], 'result': {'result': 'F89DB9D2C9'}}, 'worker4': {'events': ['commit B46A9F6475', 'previous AA54E1A168', 'commit 0E7D6FFF48', 'previous A2966E1D93', 'commit 27BED6297F'], 'result': {'result': 'D25986168C'}}, 'worker5': {'events': ['commit FB41816C0E', 'previous 88E9380BB8', 'commit CFFEFAF557', 'previous 36D76BCFA1', 'commit D2042368E2'], 'result': {'result': 'E432F6AB6B'}}, 'worker6': {'events': ['commit 60B6B3C803', 'previous 8F81336B99', 'commit 8BED933105', 'previous 74C3F8E2CC', 'commit DBA9792A48'], 'result': {'result': '3F1153AC71'}}, 'worker7': {'events': ['commit 068B2C2D15', 'previous CFD35C9A47', 'commit 5199BE503F', 'previous AFF332196E', 'commit 55EEDEF9E1'], 'result': {'result': '2C7909FA41'}}}
ANSWER = 'worker0=01B476601E\nworker1=973E9AAAB2\nworker2=2242854551\nworker3=F89DB9D2C9\nworker4=D25986168C\nworker5=E432F6AB6B\nworker6=3F1153AC71\nworker7=2C7909FA41\n'
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
