#!/usr/bin/env bash
# The README's example, run for real with sh, printf, echo, cat and ls: what
# CI does on Linux and macOS. Run from the repository's root after building
# fux3 (FUX3_BIN may name the directory holding it).
set -euo pipefail
PATH="${FUX3_BIN:-$PWD/target/debug}:$PATH"
export FUX3_DIR
FUX3_DIR=$(mktemp -d)
chmod 700 "$FUX3_DIR"
W=$(mktemp -d)
cleanup() {
    for pane in $(fux3 ls 2>/dev/null); do echo kill > "$FUX3_DIR/$pane/ctl" || true; done
    sleep 1
    rm -rf "$FUX3_DIR" "$W"
}
trap cleanup EXIT
# Waits up to five seconds for a command to succeed.
until_ok() {
    for _ in $(seq 100); do "$@" && return 0; sleep 0.05; done
    "$@"
}

D=$(fux3 dir)
[ "$D" = "$FUX3_DIR" ]
fux3 start build -- sh >/dev/null
printf 'echo hi\n' > "$D/build/in"
until_ok grep -qx hi "$D/build/screen"
head -1 "$D/build/screen" | grep -q '^id=[0-9a-f]\{16\} version=[0-9]* '
[ "$(head -1 "$D/build/screen")" = "$(head -1 "$D/build/screen.styled")" ] || {
    # Published together; a reader may catch one between the two renames.
    sleep 0.2
    [ "$(head -1 "$D/build/screen")" = "$(head -1 "$D/build/screen.styled")" ]
}
printf 'printf "\\033[31mred\\033[0m\\n"\n' > "$D/build/in"
until_ok grep -q "$(printf '\033\\[31mred')" "$D/build/screen.styled"
printf 'seq 1 100\n' > "$D/build/in"
until_ok grep -qx 50 "$D/build/history"
grep -qx 'state running' "$D/build/status"

echo "resize 100 30" > "$D/build/ctl"
until_ok grep -qx 'size 100 30' "$D/build/status"
echo "resize 0 30" > "$D/build/ctl"
until_ok grep -q '^2 resize 0 30: ' "$D/build/errors"
grep -qx 'size 100 30' "$D/build/status"

# A send of 300 KB arrives whole: cat writes it back out, counted by wc.
seq 1 50000 > "$W/paste.txt"
printf 'stty -echo; wc -c > %s/count\n' "$W" > "$D/build/in"
sleep 0.3
echo "send $W/paste.txt" > "$D/build/ctl"
printf '\004' > "$D/build/in"
until_ok test -s "$W/count"
[ "$(tr -d ' ' < "$W/count")" = "$(wc -c < "$W/paste.txt" | tr -d ' ')" ]

[ "$(ls "$D")" = build ]

# A program that ends: its exit in status, its last screen kept, typing
# after taken and dropped with a line in errors.
fux3 start short -- sh -c 'echo last words; exit 3' >/dev/null
until_ok grep -qx 'state exited 3' "$D/short/status"
grep -qx 'last words' "$D/short/screen"
printf 'ignored\n' > "$D/short/in"
until_ok grep -q '^in: 8 bytes dropped' "$D/short/errors"
echo kill > "$D/short/ctl"
until_ok test ! -e "$D/short"

echo kill > "$D/build/ctl"
until_ok test ! -e "$D/build"
[ -z "$(fux3 ls)" ]
echo "the example ran"
