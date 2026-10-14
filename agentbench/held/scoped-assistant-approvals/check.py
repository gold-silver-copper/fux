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
        archive = 'ARCHIVE v1\nkeep: winter invoices\nkeep: signed receipts\n'
        result = {'project': 'linen', 'revision': 12, 'archive_entries': 2, 'preserved': 'archive.txt'}
        payload = json.dumps(result, sort_keys=True) + '\n'
        digest = hashlib.sha256(payload.encode()).hexdigest()[:16]
        tok = lambda s: hashlib.sha256(s.encode()).hexdigest()[:12]
        conversation = ['allow scan', 'revise: preserve archive; regenerate index', 'approve ' + tok('preserve archive plan 40') + ' write:index.json', 'provide metadata linen 12', 'apply ' + tok('apply index 40:' + digest), 'exit']
        exact('archive.txt', archive)
        exact('metadata.json', '{"project":"linen","revision":12}\n')
        exact('index.json', payload)
        require(record('assistant') == {'status': 0, 'conversation': conversation, 'digest': digest, 'scope': 'write:index.json', 'preserved': True}, 'assistant conversation or permission scope incorrect')
        exact('answer.txt', 'index=' + digest + '\n')
        return True, 'all expected outputs and fixture records match'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
