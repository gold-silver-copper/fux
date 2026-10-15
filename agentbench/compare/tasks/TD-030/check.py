import hashlib
import json
from pathlib import Path

def token(label, size=10):
    return hashlib.sha256(('terminal-lab-6107:' + label).encode()).hexdigest()[:size]

def record(work, name):
    r = json.loads((Path(work) / '.fx' / (name + '.json')).read_text())
    if r.get('runs') != 1 or r.get('tty') is not True or r.get('early_input'):
        raise ValueError(name + ': not one valid terminal run')
    if r.get('state') != 'done':
        raise ValueError(name + ': not completed')
    if (Path(work) / '.fx' / (name + '.pid')).read_text() != str(r['pid']) + '\n':
        raise ValueError(name + ': missing pid record')
    return r

def answer(work, expected):
    if (Path(work) / 'answer.txt').read_bytes() != expected.encode():
        raise ValueError('answer.txt differs from the exact required lines')

def check(work: str) -> tuple[bool, str]:
    try:
        b, t, s = [record(work, 'pipeline-' + role) for role in ('build', 'test', 'serve')]
        artifact, seal, bound = token('pipeline-artifact', 20), token('pipeline-first-verdict', 18), token('pipeline-first-bound', 16)
        endpoint = 'local-' + token('pipeline-endpoint', 6)
        publication = token('pipeline-publication:' + artifact + ':' + seal + ':' + bound, 22)
        if b.get('inputs') != ['build ' + token('pipeline-build-challenge', 10), 'stop ' + artifact] or t.get('inputs') != ['test ' + artifact, 'stop ' + seal] or s.get('inputs') != ['serve ' + artifact + ' ' + seal, 'publish ' + artifact + ' ' + seal + ' ' + bound]:
            raise ValueError('handoffs did not use the first live results')
        if b.get('artifact') != artifact or t.get('verdict') != 'DEFER' or t.get('seal') != seal or [s.get('endpoint'), s.get('bound'), s.get('publication')] != [endpoint, bound, publication]:
            raise ValueError('wrong deterministic pipeline result')
        if not all((b.get('stopped_after_test'), t.get('stopped_after_serve'), t.get('started_after_build'), s.get('started_after_test'), s.get('published_while_holding'))):
            raise ValueError('readiness or shutdown order missing')
        if s.get('concurrent_terminals') != [b['terminal'], t['terminal'], s['terminal']] or len(set(s['concurrent_terminals'])) != 3 or s.get('concurrent_pids') != [b['pid'], t['pid'], s['pid']]:
            raise ValueError('three distinct overlapping terminals were not recorded')
        answer(work, 'artifact=' + artifact + '\ntest=DEFER/' + seal + '\nendpoint=' + endpoint + '/' + bound + '\npublication=' + publication + '\n')
        return True, 'three concurrent stages published and shut down in order'
    except (OSError, ValueError, KeyError, TypeError) as e:
        return False, str(e)
