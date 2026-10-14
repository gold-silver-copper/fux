from pathlib import Path
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import os
import time
import util as u
from terminal import Terminal

u.init(sys.argv[1], 'secure-console')
expected = (Path(sys.argv[1]) / 'release-note.txt').read_bytes()
password = 'ember:' + str(sum(expected) % 97)

def prompt(text):
    u.out(text + '\r')

with Terminal() as term:
    prompt('TARGET: development [selected] / staging / production; use arrows, Enter')
    selected = 0
    arrows = 0
    while True:
        k = term.key()
        if k in (b'\x1b[C', b'\x1b[B'):
            selected = (selected + 1) % 3
            arrows += 1
        elif k in (b'\x1b[D', b'\x1b[A'):
            selected = (selected - 1) % 3
            arrows += 1
        elif k == b'\r':
            break
        else:
            u.fail('Unexpected selection key')
    if selected != 1 or not arrows:
        u.fail('Select staging using arrows')
    u.event('target', target='staging')
    prompt('SHORTCUT: press Alt-v')
    if term.key() != b'\x1bv':
        u.fail('Expected Alt-v')
    u.event('alt-v')
    prompt('TOOLS: press F2')
    if term.key() != b'\x1bOQ':
        u.fail('Expected F2')
    u.event('f2')
    prompt('PASSWORD (hidden; enter ' + password + '):')
    if term.line(secret=True) != password:
        u.fail('Incorrect password')
    u.event('password', password_ok=True)
    time.sleep(1.5)
    os.write(1, b'\x1b[?2004h')
    prompt('PASTE NOTE now (CR or LF; bracketed paste optional):')
    pasted = bytearray()
    submit_shown = False
    while True:
        k = term.key()
        if k == b'\x04':
            break
        if k == b'\x1b[200~':
            pasted.extend(term.paste())
            paste_complete = True
        else:
            paste_complete = False
            if k == b'\r':
                pasted.extend(b'\n')
            elif k == b'\x15':
                start = pasted.rfind(b'\n') + 1
                del pasted[start:]
            elif k in (b'\x7f', b'\x08'):
                if pasted:
                    pasted.pop()
            elif len(k) == 1 and k >= b' ':
                pasted.extend(k)
            else:
                u.fail('Unexpected note input')
        if not submit_shown and (paste_complete or len(pasted) >= len(expected)):
            prompt('SUBMIT with Ctrl-D')
            submit_shown = True
    if bytes(pasted) != expected:
        u.fail('Submitted note differs from the source')
    os.write(1, b'\x1b[?2004l')
    u.event('paste', paste_text=pasted.decode(), paste_bytes=len(pasted))
    u.event('ctrl-d')
u.out('Note accepted; bytes=' + str(len(expected)))
u.finish(0, accepted=True)
