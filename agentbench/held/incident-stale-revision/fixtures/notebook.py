import sys
sys.dont_write_bytecode = True
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *
f = Fixture(sys.argv[1], 'notebook')
data = json.loads((f.work / 'tickets.json').read_text())
seed = (f.work / 'notebook-seed.txt').read_text().strip()
approval = digest('D-07:' + seed)
help_lines = [f'Guide {i:02d}: incident workflows and policy reference.' for i in range(65)]
help_lines[46] = 'DEPENDENCY D-07 approval key=' + approval
print('\x1b[?1049h', end='', flush=True)
atexit.register(lambda: print('\x1b[?1049l', end='', flush=True))
screen(['Synchronizing incident notebook ...'])
f.pause(1.9)
mode, pattern, query, selected, page = 'list', '', '', 0, 0
help_seen, reloaded, conflict, approved = False, False, False, False
field, severity, owner, code, editing = 0, 0, '', '', None
severities = ['low', 'medium', 'high']
while True:
    matches = [x for x in data if pattern.lower() in (x['id'] + ' ' + x['description']).lower()]
    if mode == 'list':
        selected = min(selected, max(0, len(matches)-1))
        screen(['Incident notebook -- / filter; ? help; Enter opens', 'Filter: ' + pattern] +
               [('> ' if i == selected else '  ') + x['id'] + ' | ' + x['description'] for i, x in enumerate(matches[:15])])
    elif mode == 'help':
        if page <= 46 < page + 18: help_seen = True
        screen(['HELP PAGER -- PageDown/PageUp; q returns', f'Lines {page}-{min(page+17,64)}'] + help_lines[page:page+18])
    elif mode == 'search':
        screen(['Filter incidents (ID or description)', 'Filter input: ' + query, 'Enter applies; Escape cancels'])
    elif mode == 'detail':
        screen(['Incident ' + editing['id'], 'Description: ' + editing['description'],
                'Severity: ' + editing['severity'], 'Owner: ' + editing['owner'],
                'Revision: ' + str(editing['revision']), 'e edits; q returns'])
    elif mode == 'edit':
        screen(['EDIT ' + editing['id'] + ' revision=' + str(editing['revision']), 'Tab changes field; arrows select severity; Ctrl-U clears; Ctrl-S saves',
                ('> ' if field == 0 else '  ') + 'Severity: ' + severities[severity],
                ('> ' if field == 1 else '  ') + 'Owner: ' + owner,
                ('> ' if field == 2 else '  ') + 'D-07 approval: ' + code])
    elif mode == 'conflict':
        screen([f'STALE REVISION -- another editor published revision {remote_revision}', 'R reloads; unsaved field changes will be discarded'])
    elif mode == 'confirm':
        screen(['WRITE CONFIRMATION ' + editing['id'], 'Arrows choose; Enter applies',
                ('  No      [Yes]' if approved else '[No]       Yes')])
    k = key()
    if isinstance(k, tuple):
        _, button, x, y, pressed = k
        if not pressed:
            continue
        if mode == 'help' and button in (64, 65):
            k = 'PAGEUP' if button == 64 else 'PAGEDOWN'
        elif button == 0:
            if mode == 'list' and 3 <= y <= min(17, len(matches) + 2):
                selected = y - 3
            elif mode == 'edit' and 3 <= y <= 5:
                field = y - 3
            elif mode == 'confirm' and y == 3:
                approved = x >= 12
            elif mode == 'detail' and y == 6:
                k = 'e'
            else:
                continue
            if isinstance(k, tuple): continue
        else:
            continue
    if mode == 'list':
        if k == '?': mode, page = 'help', 0
        elif k == '/': mode, query = 'search', ''
        elif k == 'DOWN': selected = min(selected+1, len(matches)-1)
        elif k == 'UP': selected = max(0, selected-1)
        elif k in ('\r', '\n') and matches:
            editing, mode = matches[selected], 'detail'
    elif mode == 'help':
        if k == 'PAGEDOWN': page = min(page+18, 54)
        elif k == 'PAGEUP': page = max(0, page-18)
        elif k == 'q': mode = 'list'
    elif mode == 'search':
        if k in ('\r','\n'): pattern, mode, selected = query, 'list', 0
        elif k == 'ESC': mode = 'list'
        elif k == '\x15': query = ''
        elif k in ('\x7f','\x08'): query = query[:-1]
        elif len(k) == 1 and k.isprintable(): query += k
    elif mode == 'detail':
        if k == 'q': mode = 'list'
        elif k == 'e':
            severity, owner, code, field, mode = severities.index(editing['severity']), editing['owner'], '', 0, 'edit'
    elif mode == 'edit':
        if k == '\t': field = (field+1) % 3
        elif k == 'BACKTAB': field = (field-1) % 3
        elif field == 0 and k == 'DOWN': severity = min(2, severity+1)
        elif field == 0 and k == 'UP': severity = max(0, severity-1)
        elif field in (1,2) and k == '\x15':
            if field == 1: owner = ''
            else: code = ''
        elif field in (1,2) and k in ('\x7f', '\x08'):
            if field == 1: owner = owner[:-1]
            else: code = code[:-1]
        elif k == '\x13':
            if editing['id'] != 'Q-24' or severities[severity] != 'high' or owner != 'ivy' or code != approval or not help_seen:
                f.fail('wrong incident, edited values, or approval key')
            attempts = f.state.get('attempts', []) + [{'id':editing['id'], 'severity':severities[severity], 'owner':owner, 'code':code, 'revision':editing['revision']}]
            f.save(attempts=attempts)
            screen(['Validating revision ...'])
            f.pause(.9)
            if not conflict:
                remote_revision = editing['revision'] + 1
                conflict, mode = True, 'conflict'
            else:
                mode, approved = 'confirm', False
        elif field in (1,2) and len(k) == 1 and k.isprintable():
            if field == 1: owner += k
            else: code += k
    elif mode == 'conflict':
        if k in ('r','R'):
            editing['revision'] = remote_revision
            reloaded = True
            severity, owner, code, field, mode = severities.index(editing['severity']), editing['owner'], '', 0, 'edit'
    elif mode == 'confirm':
        if k in ('RIGHT','LEFT'): approved = not approved
        elif k in ('\r','\n'):
            if not approved: mode = 'edit'
            else:
                if not reloaded: f.fail('reload required')
                editing['severity'], editing['owner'] = severities[severity], owner
                receipt = digest(seed + ':Q-24:high:ivy:' + str(editing['revision']) + ':' + approval)
                f.done(tickets=data, help_seen=help_seen, reloaded=reloaded, modal_approved=True, revision=editing['revision'], receipt=receipt)
                print('\x1b[?1049l', end='', flush=True)
                print('NOTEBOOK SAVED receipt=' + receipt, flush=True)
                break
