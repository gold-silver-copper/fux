import json, os
from pathlib import Path
EXPECTED = {'runner': {'events': ['run 14CB1BE367', 'run 3A33C8D5F1', 'run C0959F2B54', 'run 60A715FF54', 'run 22C65E5F4D', 'run 1FED01F92D', 'run 2EB568A4AB', 'run 49CC59866F', 'continue-unwatched', 'run 518C2575F6', 'run A2C7945A6D', 'run F89DC24674', 'run 74A11C2834', 'run 69E71EBB67', 'run 554E2A9F4B'], 'result': {'ledger': 'CF0D375E57'}}, 'inspector': {'events': ['inspect 37C7AE48DB', 'inspect 4F72AFDB32', 'inspect AE978A352C', 'inspect FE9AA995CE', 'inspect 42FE90405E'], 'result': {'diagnosis': '320BCB0D3F'}}, 'repairer': {'events': ['repair 320BCB0D3F', 'repair 6E4CC70611', 'repair 64C94D264D', 'repair BFC92C4671', 'repair 7AB018690F'], 'result': {'repair': '0120AAB8B2'}}}
ANSWER = 'checkpoint=37C7AE48DB\ndiagnosis=320BCB0D3F\nrepair=0120AAB8B2\nledger=CF0D375E57\n'
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
