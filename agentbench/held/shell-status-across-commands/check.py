import json
from pathlib import Path


SEED = 17
calculation_status = 10 + SEED % 10
expected_total = sum([11, -3, 29, 5])
receipt = f'SH-{expected_total}-{calculation_status}'

def check(work: str) -> tuple[bool, str]:
    root = Path(work)
    try:
        records = {'batch_begin': {'exit': 0, 'inputs': [], 'initialized': True}, 'batch_calculate': {'exit': calculation_status, 'inputs': [], 'cwd': 'queue stage', 'label': 'morning shift', 'same_shell': True, 'total': expected_total}, 'batch_finalize': {'exit': 0, 'inputs': [], 'cwd': 'queue stage', 'label': 'morning shift', 'same_shell': True, 'reported': calculation_status, 'total': expected_total, 'receipt': receipt}}
        for name, expected in records.items():
            value = json.loads((root / '.fx' / (name + '.json')).read_text())
            if not isinstance(value, dict):
                return False, f'{name}: invalid fixture record'
            for key, wanted in dict(ok=True, tty=True, stdin_tty=True, stdout_tty=True, attempts=1, finished=True, **expected).items():
                if key not in value or json.dumps(value[key], sort_keys=True) != json.dumps(wanted, sort_keys=True):
                    return False, f'{name}: incorrect {key}'
        if (root / 'answer.txt').read_text() != f'sum={expected_total}\nexit={calculation_status}\nreceipt={receipt}\n':
            return False, 'answer.txt does not match the required result'
        pids = [json.loads((root / '.fx' / ('batch_' + stage + '.json')).read_text())['shell_pid'] for stage in ('begin', 'calculate', 'finalize')]
        if len(set(pids)) != 1:
            return False, 'commands did not use one persistent shell'
    except (OSError, ValueError, TypeError, KeyError) as error:
        return False, 'missing or invalid result: ' + str(error)
    return True, 'all recorded actions and the final answer match'

if __name__ == '__main__':
    import sys
    passed, reason = check(sys.argv[1])
    print(('PASS: ' if passed else 'FAIL: ') + reason)
    raise SystemExit(0 if passed else 1)
