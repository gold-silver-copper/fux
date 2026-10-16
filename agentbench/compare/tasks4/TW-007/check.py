import json, os
from pathlib import Path
EXPECTED = {'primary': {'events': ['arm', 'retire'], 'result': {'outcome': 'degraded'}}, 'backup': {'events': ['arm', 'replay 091F8B2EE6', 'replay E049100359', 'replay 51F30D4E35', 'replay DA6D2034F8', 'replay 8BE13CA75F', 'replay 0FB0E1A549', 'replay 32891EA2D8', 'replay 2CE478F478', 'interrupt:redraw', 'stop B20D47F8B2'], 'result': {'ticket': '87776F600E'}}, 'consumer': {'events': ['choose backup 87776F600E', 'consume 39E93B14E2', 'consume F1D1FEC8E5', 'consume 1F442CD4AC', 'consume 45CFFF7FBC', 'consume 4724AB7C02', 'consume 3B5B858F8D', 'consume E9D41F53AC', 'consume 1F89745F55'], 'result': {'digest': '2826D22D63'}}}
ANSWER = 'provider=backup\nticket=87776F600E\ndigest=2826D22D63\nprimary=degraded\n'
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
