#!/bin/sh
# Fails if fux has outgrown its ceilings, in lines written by hand:
#   the emulator (src/term.rs, src/width.rs and the tools that write its tables)   2,500
#   the whole program (src/ without its tests, and the key scripts in keys/)       5,500
# A table a tool wrote (in src/, from `#[rustfmt::skip]` to the `];` that ends it) is
# counted apart: it grows with Unicode, not with what fux does. CI runs this.
set -eu
cd "$(dirname "$0")"
tables() { awk '/^#\[rustfmt::skip\]$/ { t = 1 } t { n++ } /^\];$/ { t = 0 } END { print n + 0 }' "$@"; }
lines() { cat "$@" | wc -l; }
generators=$(find tools -maxdepth 1 -type f)
program=$(find src -name '*.rs' ! -name tests.rs)
emulator=$(($(lines src/term.rs src/width.rs $generators) - $(tables src/term.rs src/width.rs)))
whole=$(($(lines $program) - $(tables $program) + $(find keys -type f -exec cat {} + | wc -l)))
echo "sizes: emulator $emulator of 2500, program $whole of 5500, generated tables $(tables $program) apart"
[ "$emulator" -le 2500 ] && [ "$whole" -le 5500 ]
