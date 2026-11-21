#!/bin/sh
# Fails if the parts a second program could one day use on their own name
# anything else of this crate: the emulator (term.rs, which may name width.rs and sixel.rs),
# the system calls (sys.rs), the key names (keys.rs) and the cutter of sixel
# pictures (sixel.rs). Each is compiled here as a crate of its own, so the
# check is the compiler's. CI runs it.
set -eu
cd "$(dirname "$0")/src"
out=$(mktemp -d) && trap 'rm -rf "$out"' EXIT
alone() { rustc --edition 2024 -A dead_code --crate-type lib --emit metadata "$@"; }
printf '#[path = "%s/width.rs"]\npub mod width;\n#[path = "%s/sixel.rs"]\npub mod sixel;\n#[path = "%s/term.rs"]\npub mod term;\n' \
    "$PWD" "$PWD" "$PWD" |
    alone -F unsafe-code --crate-name emulator -o "$out/emulator.rmeta" -
alone -F unsafe-code --crate-name widths -o "$out/widths.rmeta" width.rs
alone --crate-name system -o "$out/system.rmeta" sys.rs
alone -F unsafe-code --crate-name keys -o "$out/keys.rmeta" keys.rs
alone -F unsafe-code --crate-name sixel -o "$out/sixel.rmeta" sixel.rs
echo "boundary: ok"
