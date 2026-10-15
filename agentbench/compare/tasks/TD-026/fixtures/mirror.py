import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

f = Fixture(sys.argv[1], 'mirror')
f.out('\x1b[36mMirror routing console / rehearsal buffer\x1b[0m')
results = []
for round_no in (1, 2):
    n = str(round_no)
    arm = token('mirror-arm-' + n)
    route = ('fallback', 'primary')[round_no - 1]
    ticket = token('mirror-first-' + n, 14)
    f.out('ROUTE race=' + n + ' route=archive ticket=' + token('mirror-rehearsal-' + n, 14))
    f.out('Preparing the next race; keyboard input is not yet accepted.')
    f.guard(2.4 if round_no == 1 else 4.1)
    f.expect('race=' + n + ': type arm ' + arm, 'arm ' + arm)
    f.out('ARM ACCEPTED race=' + n)
    f.out('LIVE BEGIN race=' + n)
    f.guard(7.3 if round_no == 1 else 3.7)
    f.out('\x1b[32mROUTE race=' + n + ' route=' + route + ' ticket=' + ticket + '\x1b[0m', end='')
    f.guard(.28)
    f.out('\r\x1b[2KRechecking route cache...', end='')
    f.guard(1.1)
    f.out('\r\x1b[2KROUTE race=' + n + ' route=' + ('primary' if route == 'fallback' else 'fallback') +
          ' ticket=' + token('mirror-later-' + n, 14))
    f.expect('race=' + n + ': type retain <first route> <its ticket>', 'retain ' + route + ' ' + ticket)
    results.append(route + '/' + ticket)
    f.out('RACE RETAINED race=' + n)
f.done(results=results)
f.out('MIRRORS CLOSED')
