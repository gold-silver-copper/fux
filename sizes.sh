#!/bin/sh
# Prints how large fux is, in lines written by hand (no ceiling: each change says its lines):
#   the emulator (src/term.rs, src/width.rs and the tools that write its tables)
#   the whole program (src/ without its tests, and the key scripts in keys/)
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
echo "sizes: emulator $emulator, program $whole, generated tables $tables apart"
