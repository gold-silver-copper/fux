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
printf 'echo hi\n' > "$D/main/build/in"
until_ok grep -qx hi "$D/main/build/screen"
head -1 "$D/main/build/screen" | grep -q '^id=[0-9a-f]\{16\} version=[0-9]* '
[ "$(head -1 "$D/main/build/screen")" = "$(head -1 "$D/main/build/screen.styled")" ] || {
    # Published together; a reader may catch one between the two renames.
    sleep 0.2
    [ "$(head -1 "$D/main/build/screen")" = "$(head -1 "$D/main/build/screen.styled")" ]
}
printf 'printf "\\033[31mred\\033[0m\\n"\n' > "$D/main/build/in"
until_ok grep -q "$(printf '\033\\[31mred')" "$D/main/build/screen.styled"
printf 'seq 1 100\n' > "$D/main/build/in"
until_ok grep -qx 50 "$D/main/build/history"
grep -qx 'state running' "$D/main/build/status"

echo "resize 100 30" > "$D/main/build/ctl"
until_ok grep -qx 'size 100 30' "$D/main/build/status"
echo "resize 0 30" > "$D/main/build/ctl"
until_ok grep -q '^2 resize 0 30: ' "$D/main/build/errors"
grep -qx 'size 100 30' "$D/main/build/status"

# A send of 300 KB arrives whole: cat writes it back out, counted by wc.
seq 1 50000 > "$W/paste.txt"
printf 'stty -echo; wc -c > %s/count\n' "$W" > "$D/main/build/in"
sleep 0.3
echo "send $W/paste.txt" > "$D/main/build/ctl"
printf '\004' > "$D/main/build/in"
until_ok test -s "$W/count"
[ "$(tr -d ' ' < "$W/count")" = "$(wc -c < "$W/paste.txt" | tr -d ' ')" ]

[ "$(ls "$D")" = main ]
[ "$(ls "$D/main")" = build ]

# The viewer, run in a pane of a session of its own (it needs a terminal),
# and the layout driven with a plain file while it runs.
V=$(mktemp -d); chmod 700 "$V"
FUX3_DIR=$V fux3 start viewer --size 100x30 -- env FUX3_DIR="$D" fux3 view >/dev/null
until_ok test -p "$D/main/.view/ctl"
echo "split build right" > "$D/main/.view/ctl"
until_ok grep -qs 'pane shell-1@' "$D/main/.view/layout"
until_ok test -d "$D/main/shell-1"
grep -qx 'size 100 30' "$V/main/viewer/status"
# Killing the viewer outright leaves every pane running and typable.
kill -9 "$(sed -n 's/^pid //p' "$V/main/viewer/status")"
until_ok grep -q '^state signalled KILL' "$V/main/viewer/status"
[ "$(fux3 ls)" = "$(printf 'main/build\nmain/shell-1')" ]
printf 'echo after-viewer\n' > "$D/main/shell-1/in"
until_ok grep -qx after-viewer "$D/main/shell-1/screen"
echo kill > "$D/main/shell-1/ctl"
until_ok test ! -e "$D/main/shell-1"
echo kill > "$V/main/viewer/ctl"
until_ok test ! -e "$V/main/viewer"
rm -rf "$V"

# A program that ends: its exit in status, its last screen kept, typing
# after taken and dropped with a line in errors.
fux3 start short -- sh -c 'echo last words; exit 3' >/dev/null
until_ok grep -qx 'state exited 3' "$D/main/short/status"
grep -qx 'last words' "$D/main/short/screen"
printf 'ignored\n' > "$D/main/short/in"
until_ok grep -q '^in: 8 bytes dropped' "$D/main/short/errors"
echo kill > "$D/main/short/ctl"
until_ok test ! -e "$D/main/short"

echo kill > "$D/main/build/ctl"
until_ok test ! -e "$D/main/build"
[ -z "$(fux3 ls)" ]
echo "the example ran"
