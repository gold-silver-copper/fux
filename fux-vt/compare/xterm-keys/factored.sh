#!/bin/bash
# Records real xterm 411's bytes for keys with modifyOtherKeys's mask of
# modifiers to factor out (XTMODKEYS `CSI > 4 ; level : mask m`, KEY-24)
# into fux-vt/tests/data/keys/xterm-keys-factored.txt, as run.sh records
# xterm-keys.txt: xterm on a private Xvfb display (:198), the keys typed
# through XTest (inject.c), xterm's reads kept by capture.py; the fux keys
# named as xterm-keys.txt names them at level 2. About a minute. Also
# records `CSI > 4 ; 2 m CSI > 4 : 1 m` (ctlseqs' example form) to
# $TMPDIR, which xterm 411 ignores (extended.rs checks fux-vt does too).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=$here/../../tests/data/keys
[[ $(xterm -version) == "XTerm(411)" ]] || { echo "factored.sh: needs xterm 411" >&2; exit 2; }
work=$(mktemp -d "${TMPDIR:?factored.sh: set TMPDIR off /tmp}/xterm-factored.XXXXXX")
cc -O2 -o "$work/inject" "$here/inject.c" -lX11 -lXtst
display=:198
Xvfb $display -screen 0 1024x768x24 -nolisten tcp -nolisten unix -nolock >"$work/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill $xvfb 2>/dev/null; rm -rf "$work"' EXIT
sleep 1.5
declare -A setup=([m2s]='\x1b[>4;2:1m' [m2c]='\x1b[>4;2:4m' [m2a]='\x1b[>4;2:8m'
  [m2cs]='\x1b[>4;2:5m' [m1s]='\x1b[>4;1:1m' [m1c]='\x1b[>4;1:4m' [m3s]='\x1b[>4;3:1m'
  [ct]='\x1b[>4;2m\x1b[>4:1m')
for mode in m2s m2c m2a m2cs m1s m1c m3s ct; do
  DISPLAY=$display xterm -geometry 80x24+0+0 \
    -xrm "XTerm*metaSendsEscape: false" -xrm 'XTerm*eightBitInput: false' \
    -xrm 'XTerm*backarrowKey: false' -xrm 'XTerm*backarrowKeyIsErase: false' \
    -xrm 'XTerm*ptyInitialErase: false' -xrm 'XTerm*sunKeyboard: false' \
    -xrm 'XTerm*allowSendEvents: false' \
    -e python3 "$here/capture.py" "$work/$mode.reads" "${setup[$mode]}" &
  xt=$!
  for _ in $(seq 50); do [[ -s $work/$mode.reads ]] && break; sleep 0.1; done
  sleep 0.5
  for k in a 1 space Return Tab Escape BackSpace slash; do
    for m in - S C M CS CM MS CMS; do echo "$m $k"; done
  done | DISPLAY=$display "$work/inject" 100 > "$work/$mode.keys"
  tail --pid=$xt -f /dev/null
done
python3 - "$work" "$data/xterm-keys.txt" "$data/xterm-keys-factored.txt" <<'PY'
import os, sys
run, base, out = sys.argv[1:4]
def esc(b):
    o = ''
    for x in b:
        if x == 0x1b: o += '\\e'
        elif x == 0x5c: o += '\\\\'
        elif 0x20 <= x <= 0x7e: o += chr(x)
        else: o += '\\x%02x' % x
    return o
spec = {}
for l in open(base):
    f = l.rstrip('\n').split('\t')
    if not l.startswith('#') and len(f) == 5 and f[1] == 'legacy mok=2':
        spec[f[0]] = f[2]
MODE = {'m2s': 'legacy mok=2 factor=1', 'm2c': 'legacy mok=2 factor=4', 'm2a': 'legacy mok=2 factor=8',
        'm2cs': 'legacy mok=2 factor=5', 'm1s': 'legacy mok=1 factor=1', 'm1c': 'legacy mok=1 factor=4',
        'm3s': 'legacy mok=3 factor=1'}
lines = ["# Real xterm 411's bytes with modifyOtherKeys's mask of modifiers to factor",
         "# out (XTMODKEYS `CSI > 4 ; level : mask m`, ctlseqs \"Alt and Meta Keys\"),",
         "# typed through XTest as xterm-keys.txt's are (fux-vt/compare/xterm-keys:",
         "# factored.sh), each mode's sequence sent first. `factor=N` is the mask:",
         "# X's state bits, Shift 1, Control 4, Mod1 (Alt) 8. Source, mode, key,",
         "# xterm's bytes, verdict."]
for m in MODE:
    keys = [l.split() for l in open(os.path.join(run, m + '.keys')) if l.strip()]
    reads = [(float(l.split()[0]), bytes.fromhex(l.split()[1]))
             for l in open(os.path.join(run, m + '.reads')) if l[0].isdigit()]
    got = [b''] * len(keys)
    j = 0
    for t, data in reads:
        while j + 1 < len(keys) and float(keys[j + 1][0]) <= t: j += 1
        if float(keys[j][0]) <= t: got[j] += data
    for (_, mods, k), b in zip(keys, got):
        src = 'xterm 411 %s %s' % (mods, k)
        if src not in spec: continue
        verdict = 'agree'
        if not b and k == 'Return' and 'M' in mods:
            b, verdict = None, "na: xterm's own default translation takes Alt-Enter (full screen)"
        lines.append('\t'.join([src, MODE[m], spec[src], 'none' if b is None else esc(b), verdict]))
open(out, 'w').write('\n'.join(lines) + '\n')
PY
grep -h . "$work/ct.reads" | tail -n +2 > "${TMPDIR}/xterm-factored-ct.reads"
