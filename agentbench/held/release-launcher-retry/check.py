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
        bad=json.loads((w/'.fx'/'launcher-broken.json').read_bytes().decode("utf-8"))
        require(bad['exit']==23 and bad['events']==['manifest-crash'],'the failed launch was not observed')
        require((w/'.fx'/'launcher-broken.pid').read_bytes().decode("utf-8").strip().isdigit(),'broken PID missing')
        good=record('launcher-release')
        for launch in (bad,good):
            require(launch.get('stdin_tty') is True and launch.get('stdout_tty') is True, 'both launches require terminal stdin and stdout')
            require(launch['size']==[96,28] and launch['channel']=='amber' and launch['cwd']==str(w.resolve()/'project'),'launch context is wrong')
        require(good['events']==['input:publish harbor-47','published:harbor-47'],'publication handshake is wrong')
        require((w/'project'/'config.json').read_bytes().decode("utf-8")=='{"tag":"harbor-47","revision":7}\n','configuration was changed')
        answer('23\n0\nharbor-47\n')
        return True, 'All required interactions and final files match.'
    except OSError as exc:
        return False, 'Missing or unreadable file: ' + Path(exc.filename or 'state').name
    except (ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason=check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
