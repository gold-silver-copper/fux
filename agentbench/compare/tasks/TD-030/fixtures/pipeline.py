import runpy
import sys
from pathlib import Path
_lib = runpy.run_path(str(Path(__file__).with_name('common.py')))
Fixture, token, read_record = (_lib[k] for k in ('Fixture', 'token', 'read_record'))

import os
role = sys.argv[2]
if role not in ('build', 'test', 'serve'):
    raise SystemExit('role must be build, test, or serve')
f = Fixture(sys.argv[1], 'pipeline-' + role)
artifact = token('pipeline-artifact', 20)
seal = token('pipeline-first-verdict', 18)
bound = token('pipeline-first-bound', 16)
endpoint = 'local-' + token('pipeline-endpoint', 6)
publication = token('pipeline-publication:' + artifact + ':' + seal + ':' + bound, 22)

def predecessor(name, state):
    r = read_record(f.work, 'pipeline-' + name)
    if r.get('state') != state or not r.get('tty'):
        f.fail(name + ' must already be ' + state)
    if state == 'holding':
        try:
            os.kill(r['pid'], 0)
        except (OSError, KeyError):
            f.fail(name + ' is not still running')
    return r

f.out('\x1b[36mWorkshop stage: ' + role + '\x1b[0m')
if role == 'build':
    f.out('CACHED ARTIFACT ' + token('pipeline-cached-artifact', 20))
    f.guard(3.4)
    challenge = token('pipeline-build-challenge', 10)
    f.expect('type build ' + challenge, 'build ' + challenge)
    f.out('BUILD ACCEPTED')
    for n in range(3):
        f.out('\r\x1b[2KCompiling module ' + str(n + 1) + '/3', end='')
        f.guard(2.1)
    f.out('\r\x1b[2KARTIFACT ' + artifact)
    f.hold(artifact=artifact)
    f.out('BUILD HOLDING')
    f.expect('after TEST CLOSED, type stop <live artifact>', 'stop ' + artifact)
    predecessor('test', 'done')
    f.done(artifact=artifact, stopped_after_test=True)
    f.out('BUILD CLOSED')
elif role == 'test':
    b = predecessor('build', 'holding')
    if b['terminal'] == f.r['terminal']:
        f.fail('build and test require separate concurrent terminals')
    f.r['started_after_build'] = True
    f.save()
    f.out('CACHED VERDICT PASS seal=' + token('pipeline-cached-verdict', 18))
    f.guard(2.8)
    f.expect('type test <live artifact>', 'test ' + artifact)
    f.out('TEST ACCEPTED')
    f.guard(6.7)
    f.out('\x1b[33mVERDICT DEFER seal=' + seal + '\x1b[0m', end='')
    f.guard(.26)
    f.out('\r\x1b[2KRetrying advisory checks...', end='')
    f.guard(.9)
    f.out('\r\x1b[2KVERDICT PASS seal=' + token('pipeline-late-verdict', 18))
    f.hold(verdict='DEFER', seal=seal)
    f.out('TEST HOLDING')
    f.expect('after PUBLISH CLOSED, type stop <first verdict seal>', 'stop ' + seal)
    predecessor('serve', 'done')
    f.done(verdict='DEFER', seal=seal, stopped_after_serve=True)
    f.out('TEST CLOSED')
else:
    b, t = predecessor('build', 'holding'), predecessor('test', 'holding')
    terminals = [b['terminal'], t['terminal'], f.r['terminal']]
    if len(set(terminals)) != 3:
        f.fail('all three stages require separate concurrent terminals')
    f.r.update(started_after_test=True, concurrent_terminals=terminals,
               concurrent_pids=[b['pid'], t['pid'], f.r['pid']])
    f.save()
    f.out('CACHED BOUND endpoint=local-cache ticket=' + token('pipeline-cached-bound', 16))
    f.guard(4.2)
    f.expect('type serve <live artifact> <first verdict seal>', 'serve ' + artifact + ' ' + seal)
    f.out('SERVE ACCEPTED')
    f.guard(5.3)
    f.out('\x1b[32mBOUND endpoint=' + endpoint + ' ticket=' + bound + '\x1b[0m', end='')
    f.guard(.29)
    f.out('\r\x1b[2KRepainting listener display...', end='')
    f.guard(.8)
    f.out('\r\x1b[2KBOUND endpoint=local-spare ticket=' + token('pipeline-late-bound', 16))
    f.expect('type publish <live artifact> <first verdict seal> <first bound ticket>',
             'publish ' + artifact + ' ' + seal + ' ' + bound)
    predecessor('build', 'holding')
    predecessor('test', 'holding')
    f.done(endpoint=endpoint, bound=bound, publication=publication, published_while_holding=True)
    f.out('PUBLICATION ' + publication)
    f.out('PUBLISH CLOSED')
