#!/bin/bash
# Records real xterm 411's bytes for every key fux names, in every legacy
# mode fux-vt encodes, into fux-vt/tests/data/keys/xterm-keys.txt:
# xterm on a private Xvfb display (:197, no socket or lock file written),
# the keys typed through XTest, about 6 minutes. Needs xterm 411, Xvfb and
# libXtst. Verdicts are kept from the file as it is: a new or changed case
# comes in as `agree`, and input_suites.rs then says where fux-vt differs.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=$here/../../tests/data/keys/xterm-keys.txt
[[ $(xterm -version) == "XTerm(411)" ]] || { echo "run.sh: needs xterm 411" >&2; exit 2; }
work=$(mktemp -d "${TMPDIR:?run.sh: set TMPDIR off /tmp}/xterm-keys.XXXXXX")
cc -O2 -o "$work/inject" "$here/inject.c" -lX11 -lXtst
cp "$here"/*.py "$here/drive.sh" "$work/"
"$work/drive.sh" "$work/run" legacy app bkm mok1 mok2 mok3
python3 "$here/align.py" "$work/run" "$work/new.txt"
python3 - "$data" "$work/new.txt" <<'PY'
import sys
old={'\t'.join(l.split('\t')[:4]):l.rstrip('\n').split('\t')[4] for l in open(sys.argv[1]) if l.strip() and not l.startswith('#')}
out=[]
for l in open(sys.argv[2]).read().split('\n'):
    f=l.split('\t')
    if len(f)==5 and not l.startswith('#'):
        f[4]=old.get('\t'.join(f[:4]),'agree')
    out.append('\t'.join(f))
open(sys.argv[1],'w').write('\n'.join(out))
PY
rm -rf "$work"
