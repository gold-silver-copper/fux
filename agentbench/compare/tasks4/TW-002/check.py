import json, os
from pathlib import Path
EXPECTED = {'north': {'events': ['seal 2D76819192', 'import east A717E0C810', 'seal B3A66B930F', 'import east CFE4ADA080', 'seal 7808CF59E3', 'import east 32926006C5', 'seal 2FC4BBBE2E', 'import east 68ACEF8D26', 'seal 7C065DDDD6', 'import east 8100A399AD', 'seal BF0B78A5BF', 'import east 544532407E', 'seal D1617DCFB2', 'import east 0D0A422602', 'seal EA3A0504EF', 'import east 264E4BE0BA', 'stop 630DE674A0'], 'result': {'certificate': 'F0B837BD03'}}, 'east': {'events': ['seal D40079279F', 'import west DDF6BCA10A', 'seal 1A52278157', 'import west F4C9DBF0DA', 'seal CF4FFCE050', 'import west 649164DCF2', 'seal 3F1AEABD85', 'import west 3EF885A2EB', 'seal 3B6F0091EA', 'import west E1E3225F26', 'seal BA1A7B30BB', 'import west 17AC9ED30C', 'seal 5203E96D58', 'import west 15518A309F', 'seal 6768915084', 'import west 93129A9ADB', 'stop 4388C81AE1'], 'result': {'certificate': '975D173710'}}, 'west': {'events': ['seal 4F18F295DE', 'import north 21E8A005E4', 'seal 7DD9D738A2', 'import north B1D1D6DDA0', 'seal 4E1BEF6108', 'import north 13D625B68F', 'seal DD8F96109B', 'import north B49F39085B', 'seal 08508A0CD5', 'import north E4CEBA74B7', 'seal 88798788E7', 'import north F7CCA5D92E', 'seal 20DE4B211F', 'import north 62BB1BBF08', 'seal 22E3401420', 'import north D0227A4646', 'stop 5F3D128287'], 'result': {'certificate': '822BFA6908'}}}
ANSWER = 'north=F0B837BD03\neast=975D173710\nwest=822BFA6908\n'
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
