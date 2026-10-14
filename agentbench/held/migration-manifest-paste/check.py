
import hashlib, json, os
from pathlib import Path

def token(s, n=8):
    return hashlib.sha256(s.encode()).hexdigest()[:n].upper()

def record(w, name):
    assert not (w / '.fx' / (name + '.duplicate.json')).exists(), 'fixture instance restarted'
    p = w / '.fx' / (name + '.json')
    raw = p.read_bytes()
    assert raw.endswith(b'\n') and raw.count(b'\n') == 1, 'receipt must be one newline-terminated JSON line'
    r = json.loads(p.read_text())
    assert r.get('stdin_tty') is True and r.get('stdout_tty') is True, 'fixture did not run in a terminal'
    return r

def done(r, code=0):
    return r.get('stage') == 'done' and r.get('exit_code') == code

def live(pid):
    try: os.kill(int(pid), 0); return True
    except (OSError, ValueError, TypeError): return False

def check(work: str) -> tuple[bool, str]:
    try:
        return verify(Path(work).resolve())
    except (OSError, ValueError, KeyError, TypeError, AssertionError, AttributeError, IndexError, OverflowError) as e:
        return False, 'missing or invalid final evidence: ' + str(e)

def verify(w):
    r=record(w,'migration'); payload=(w/'payload.txt').read_text()
    assert done(r) and r['password'] == (w/'credentials.txt').read_text().splitlines()[0] and r['hidden'] is True
    assert r['operation']=='Apply'
    assert r['payload']==payload
    assert r['command']=='apply --safe' and r['eof_commit'] is True
    assert r['receipt']=='M-'+token(payload+'|apply --safe')
    assert payload == 'name: "ledger$HOME"\n\npaths:\n\t- literal/$(date)\n\t- café/β\nend: "\\n is text"\n'
    return True, 'hidden input, arrow selection, literal paste, and control-key commit verified'

if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
