#!/bin/sh
# Prints how large fux is, in lines written by hand (no ceiling: each change says its lines):
#   fux-vt, the emulator (fux-vt/src without its tests)
#   fuxix, the system calls (fuxix/src)
#   fux, the program (src/ without its tests, and the key scripts in keys/)
# A table a tool wrote is counted apart: it grows with Unicode, not with what fux does.
# Such a table starts at a line `// A table written by tools/…` and ends at its `];`.
# CI runs this.
set -eu
cd "$(dirname "$0")"
count() { # count WHAT FILE…: the lines of the files that are (tables) or are not (hand) in a table
    what=$1 && shift
    awk -v what="$what" 'FNR == 1 { t = 0 } /^\/\/ A table written by tools\// { t = 1 }
        { if (t) tables++; else hand++ } /^\];$/ { t = 0 }
        END { print (what == "tables" ? tables : hand) + 0 }' "$@"
}
hand() { ls "$1"/*.rs | grep -v '/tests\.rs$'; }
vt=$(count hand $(hand fux-vt/src))
ix=$(count hand $(hand fuxix/src))
fux=$(($(count hand $(hand src)) + $(find keys -type f -exec cat {} + | wc -l)))
tables=$(count tables $(hand fux-vt/src) $(hand fuxix/src) $(hand src))
echo "sizes: fux-vt $vt, fuxix $ix, fux $fux, together $((vt + ix + fux)), generated tables $tables apart"
