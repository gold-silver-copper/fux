import json
import sys
from pathlib import Path

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    try:
        def record(name):
            data = json.loads((w / '.fx' / (name + '.json')).read_text())
            if isinstance(data, dict):
                if data.pop('stdin_tty', None) is not True or data.pop('stdout_tty', None) is not True:
                    raise ValueError(name + ' did not run with terminal stdin and stdout')
            return data
        def exact(path, expected):
            if (w / path).read_bytes() != expected.encode():
                raise ValueError('incorrect ' + path)
        def require(condition, message):
            if not condition:
                raise ValueError(message)
        import hashlib
        tok = lambda s: hashlib.sha256(s.encode()).hexdigest()[:12]
        a = tok('silver build 36'); p = tok('test proof 36:' + a); n = tok('health nonce 36:' + p)
        manifest = {'artifact': a, 'proof': p, 'health': n, 'profile': 'silver'}
        require(json.loads((w / 'manifest.json').read_text()) == manifest, 'wrong published manifest')
        require(record('build') == {'status': 0, 'artifact': a, 'released': True}, 'build did not release last')
        require(record('verify') == {'status': 0, 'artifact': a, 'proof': p, 'released': True}, 'verifier did not release after server')
        require(record('server') == dict(status=0, **manifest, published=True, closed=True), 'server publication incomplete')
        exact('answer.txt', 'health=' + n + '\nartifact=' + a + '\nproof=' + p + '\n')
        return True, 'all expected outputs and fixture records match'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
