from pathlib import Path
import json, hashlib

def check(work: str) -> tuple[bool, str]:
    w = Path(work)

    def rec(n):
        s = json.loads((w / '.fx' / (n + '.json')).read_text())
        if n not in ('inventory', 'review-report'):
            if s.pop('stdin_tty', None) is not True or s.pop('stdout_tty', None) is not True:
                raise AssertionError(n + ' did not run with terminal stdin and stdout')
        return s
    try:
        original = 'case=manifest-review\nalpha=6\nbeta=11\nkeep-source=yes\n'
        digest = hashlib.sha256(original.encode()).hexdigest()
        evidence = 'proof-' + hashlib.sha256((digest + ':offline-schema-v2').encode()).hexdigest()[:12]
        if not (w / 'case.txt').read_text() == original:
            raise AssertionError('source changed')
        if not rec('review_agent') == dict(plan='preserve-input', write='report-only', network='denied', status_inspected=True, stage='done', ok=True, evidence=evidence, exit=0):
            raise AssertionError('agent supervision incomplete')
        if not rec('offline_verifier') == dict(ok=True, evidence=evidence, source_sha256=digest, exit=0):
            raise AssertionError('offline proof invalid')
        if not rec('review-report') == dict(case='manifest-review', reviewed=17, source_sha256=digest, network='denied', method='offline-schema-v2'):
            raise AssertionError('review report wrong')
        if not (w / 'answer.txt').read_bytes() == 'reviewed=17\nsource=preserved\n'.encode():
            raise AssertionError('review summary wrong')
        return (True, 'exact output and fixture state verified')
    except FileNotFoundError as e:
        return (False, 'required file missing: ' + Path(e.filename).name)
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as e:
        return (False, (str(e) or 'final state differs from expected').splitlines()[0][:160])
if __name__ == '__main__':
    import sys
    ok, reason = check(sys.argv[1])
    print(('PASS: ' if ok else 'FAIL: ') + reason)
    raise SystemExit(0 if ok else 1)
