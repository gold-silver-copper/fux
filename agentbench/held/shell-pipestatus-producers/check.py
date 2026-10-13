import json
from pathlib import Path
import sys
import hashlib

def digest(text, n=12):
    return hashlib.sha256(text.encode()).hexdigest()[:n]

def check(work: str) -> tuple[bool, str]:
    w = Path(work)
    def record(name):
        value = json.loads((w / '.fx' / (name + '.json')).read_text())
        require(value['launch_number'] == 1 and len(list((w / '.fx').glob(name + '.launch-*.pid'))) == 1, 'program launched more than once: ' + name)
        require((w / '.fx' / (name + '.pid')).read_text() == str(value['pid']), 'pid record differs: ' + name)
        require(value['stdin_tty'] is True and value['stdout_tty'] is True, 'program did not run with terminal stdin and stdout: ' + name)
        return value
    def require(condition, reason):
        if not condition:
            raise ValueError(reason)
    def answer(expected):
        require((w / 'answer.txt').read_text() == expected, 'answer.txt differs from the exact expected text')
    try:
        for name, content in {'project/manifest.txt': 'core-lib=12; include=obsolete; target=release\n'}.items():
            require((w / name).read_text() == content, "setup file changed: " + name)
        rs = [record('build-' + s) for s in ('lint','record-lint','compile','finalize')]
        seed = (w / 'project' / 'manifest.txt').read_text().strip()
        artifact = digest(seed + ':strict-build')
        lint_status = 1 + int(digest(seed + ':lint-status', 2), 16) % 31
        compile_status = 32 + int(digest(seed + ':compile-status', 2), 16) % 31
        require(all(r['phase'] == 'complete' and r['cwd'] == str(w.resolve() / 'project') and r['flavor'] == 'strict' for r in rs), 'shell workflow incomplete or environment changed')
        require(len({r['parent'] for r in rs}) == 1, 'more than one parent shell')
        require(rs[0]['exit_code'] == rs[1]['reported_status'] == rs[3]['lint_status'] == lint_status and rs[2]['exit_code'] == rs[3]['compile_status'] == compile_status, 'producer statuses not preserved')
        require(rs[2]['artifact'] == rs[3]['artifact'] == artifact, 'wrong artifact')
        require((w / 'lint.txt').read_text() == '\x1b[33mLINT E217: obsolete include in manifest\x1b[0m\nLint finished with findings.\n', 'tee transcript differs')
        answer(f'lint={lint_status}\ncompile={compile_status}\ncwd=project\nflavor=strict\nartifact={artifact}\n')
        return True, 'all required outcomes verified'
    except (OSError, ValueError, KeyError, TypeError, IndexError) as exc:
        return False, str(exc)

if __name__ == '__main__':
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
