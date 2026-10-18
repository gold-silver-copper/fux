#!/bin/bash
# Records real xterm 411's bytes for each key of each mode, typed through
# XTest on a private Xvfb display: OUT/MODE.keys (what was typed, when) and
# OUT/MODE.reads (what xterm sent, when).
set -u
here=$(cd "$(dirname "$0")" && pwd)
out=$1; shift
display=:197
mkdir -p "$out"
Xvfb $display -screen 0 1024x768x24 -nolisten tcp -nolisten unix -nolock >"$out/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill $xvfb 2>/dev/null' EXIT
sleep 1.5
declare -A setup=([legacy]='' [app]='\x1b[?1h' [bkm]='\x1b[?67h' [mok1]='\x1b[>4;1m' [mok2]='\x1b[>4;2m' [mok3]='\x1b[>4;3m')
for mode in "$@"; do
  rm -f "$out/$mode.reads"
  DISPLAY=$display xterm -geometry 80x24+0+0 \
    -xrm "XTerm*metaSendsEscape: false" -xrm 'XTerm*eightBitInput: false' \
    -xrm 'XTerm*backarrowKey: false' -xrm 'XTerm*backarrowKeyIsErase: false' \
    -xrm 'XTerm*ptyInitialErase: false' -xrm 'XTerm*sunKeyboard: false' \
    -xrm 'XTerm*allowSendEvents: false' \
    -e python3 "$here/capture.py" "$out/$mode.reads" "${setup[$mode]}" &
  xt=$!
  for _ in $(seq 50); do [[ -s $out/$mode.reads ]] && break; sleep 0.1; done
  sleep 0.5
  python3 "$here/keys.py" "$mode" 2>/dev/null | cut -f1 | DISPLAY=$display "$here/inject" 100 > "$out/$mode.keys"
  python3 "$here/keys.py" "$mode" 2>/dev/null | cut -f2 > "$out/$mode.names"
  tail --pid=$xt -f /dev/null
done
