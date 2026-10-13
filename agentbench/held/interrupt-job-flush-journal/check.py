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
        interrupted = record('interrupted')
        terminal = interrupted['terminal']
        require(isinstance(terminal, str) and terminal.startswith('/dev/'), 'missing terminal identity')
        require(interrupted == {'status': 130, 'signal': 'SIGINT', 'armed': True, 'terminal': terminal}, 'job not interrupted after readiness')
        require(record('foreground') == {'status': 0, 'mode': 'recover', 'recovered': True, 'terminal': terminal}, 'recovery not completed in the original terminal')
        require(record('journal') == {'status': 0, 'interrupts': 1, 'signal': 'SIGTERM', 'flushed': True}, 'journal not gracefully flushed')
        exact('recovery.txt', 'rollback=complete\n')
        exact('journal.txt', 'entry=alpha\nentry=beta\ncommitted=2\n')
        exact('answer.txt', 'job=130\nrecovery=0\njournal=0\n')
        return True, 'all expected outputs and fixture records match'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(json.dumps({'passed': ok, 'reason': reason}))
    sys.exit(0 if ok else 1)
