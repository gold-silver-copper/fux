#!/usr/bin/env python3
import json, sys, hashlib
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w=Path(work)
    def record(name):
        value=json.loads((w/'.fx'/(name+'.json')).read_text())
        require(value.get('stdin_tty') is True and value.get('stdout_tty') is True, name + ' did not run with terminal stdin and stdout')
        require(value.get('exit') == 0, name + ' did not exit successfully')
        require((w/'.fx'/(name+'.pid')).read_text().strip().isdigit(), name+' PID missing')
        return value
    def require(condition, reason):
        if not condition:
            raise ValueError(reason)
    def answer(expected):
        require((w/'answer.txt').read_bytes() == expected.encode('utf-8'), 'answer.txt differs from the exact expected text')
    try:
        r=record('scanner')
        seed=402
        ticket=f'F-{seed}'
        generation=seed%11
        digest=hashlib.sha256(f'{seed}:recovered'.encode()).hexdigest()[:4]
        require(r['events']==['outcome:FAILED:'+ticket,'input:recover '+ticket,'idle:'+str(generation),'input:continue '+str(generation),'complete:'+digest],'outcome or recovery protocol is wrong')
        answer('FAILED|'+ticket+'|'+digest+'\n')
        return True, 'All required interactions and final files match.'
    except OSError as exc:
        return False, 'Missing or unreadable file: ' + Path(exc.filename or 'state').name
    except (ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason=check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
