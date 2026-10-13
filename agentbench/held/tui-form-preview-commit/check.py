
import hashlib, json, os
from pathlib import Path

def token(s, n=8):
    return hashlib.sha256(s.encode()).hexdigest()[:n].upper()

def record(w, name):
    assert not (w / '.fx' / (name + '.duplicate.json')).exists(), 'fixture instance restarted'
    p = w / '.fx' / (name + '.json')
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
    r=record(w,'console'); values=['staging','west','4','ready']; fp='FP-'+token('|'.join(values),12)
    assert done(r) and r['committed'] is True and r['values']==values
    assert r['preview']=={'values':values,'fingerprint':fp} and r['confirmed']==fp and r['fingerprint']==fp
    assert r['events'][-1]==['preview',{'values':values,'fingerprint':fp}] and r['quit_key']=='q'
    assert (w/'answer.txt').read_text()=='fingerprint='+fp+'\n'
    return True, 'previewed form, modal confirmation, and clean alternate-screen exit verified'

if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    raise SystemExit(0 if ok else 1)
