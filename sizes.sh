#!/bin/sh
# Fails if fux has outgrown its ceilings, in lines written by hand:
#   the emulator (src/term.rs, src/width.rs and the tools that write its tables)   2,500
#   the whole program (src/ without its tests, and the key scripts in keys/)       5,500
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
emulator=$(count hand src/term.rs src/width.rs tools/*.py)
program=$(ls src/*.rs | grep -v '^src/tests\.rs$')
whole=$(($(count hand $program) + $(find keys -type f -exec cat {} + | wc -l)))
tables=$(count tables $program)
echo "sizes: emulator $emulator of 2500, program $whole of 5500, generated tables $tables apart"
[ "$emulator" -le 2500 ] || { echo "sizes: the emulator is over its ceiling" >&2; exit 1; }
[ "$whole" -le 5500 ] || { echo "sizes: the program is over its ceiling" >&2; exit 1; }
