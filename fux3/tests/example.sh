#!/usr/bin/env bash
# The README's example, run for real with sh, printf, echo, cat and ls: what
# CI does on Linux and macOS. Run from the repository's root after building
# fux3 (FUX3_BIN may name the directory holding it).
set -Eeuo pipefail
PATH="${FUX3_BIN:-$PWD/target/debug}:$PATH"
export FUX3_DIR
FUX3_DIR=$(mktemp -d)
chmod 700 "$FUX3_DIR"
W=$(mktemp -d)
V=$(mktemp -d)
chmod 700 "$V"
cleanup() {
    for c in "$FUX3_DIR"/*/*/ctl "$V"/*/*/ctl; do [ -p "$c" ] && echo kill 1<>"$c"; done
    sleep 1
    rm -rf "$FUX3_DIR" "$W" "$V"
}
trap cleanup EXIT
# Says where it stopped, and what was there: CI shows nothing else.
failed() {
    echo "example.sh: failed at line $1" >&2
    for d in "$FUX3_DIR" "$V"; do
        ls -la "$d" "$d"/* >&2 || true
        for f in "$d"/*/*/status "$d"/*/*/errors "$d"/*/.view/layout; do
            [ -e "$f" ] && { echo "== $f" >&2; cat "$f" >&2; }
        done
    done
    [ -e "$V/main/viewer/screen" ] && cat "$V/main/viewer/screen" >&2
    true
}
trap 'failed $LINENO' ERR
# Waits up to five seconds for a command to succeed; until_long, twenty
# (the viewer loads its grift first; a debug build on a slow runner).
until_ok() {
    for _ in $(seq 100); do "$@" && return 0; sleep 0.05; done
    "$@"
}
until_long() {
    for _ in $(seq 400); do "$@" && return 0; sleep 0.05; done
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
# and a layout command made from outside while it runs: an edit of the
# layout file, which the viewer shows and fits within its next looks.
FUX3_DIR=$V fux3 start viewer --size 100x30 -- env FUX3_DIR="$D" fux3 view >/dev/null
printf 'echo viewer-$((1+1))\n' > "$D/main/build/in"
until_long grep -qs 'viewer-2' "$V/main/viewer/screen"
fux3 view main --do "split build right" >/dev/null
until_long grep -qs 'pane shell-1@' "$D/main/.view/layout"
until_long test -d "$D/main/shell-1"
# The split fitted build to its half of the viewer, once; the viewer's own
# pane stays the size it was started at.
until_ok grep -qx 'size 49 30' "$D/main/build/status"
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

# A program that ends: its exit in status, its last screen kept, typing
# after taken and dropped with a line in errors.
fux3 start short -- sh -c 'echo last words; exit 3' >/dev/null
until_ok grep -qx 'state exited 3' "$D/main/short/status"
grep -qx 'last words' "$D/main/short/screen"
printf 'ignored\n' > "$D/main/short/in"
until_ok grep -q '^in: 8 bytes dropped in all' "$D/main/short/errors"
echo kill > "$D/main/short/ctl"
until_ok test ! -e "$D/main/short"

# The live pane, moved to another workspace and renamed with mkdir and mv:
# still the same pane, typed into and read where it is now.
ID=$(cat "$D/main/build/id")
mkdir "$D/rig" && mv "$D/main/build" "$D/rig/server"
printf 'echo moved-$((40+2))\n' > "$D/rig/server/in"
# (This pane's echo is off since the send above: a shell without its own line
# editor then prints the answer on its prompt's line.)
until_ok grep -q moved-42 "$D/rig/server/screen"
[ "$(cat "$D/rig/server/id")" = "$ID" ]
[ "$(fux3 ls)" = rig/server ]
# A layout command with no viewer running is an edit of the layout file.
fux3 view rig --do 'tab new logs' >/dev/null
grep -q 'logs' "$D/rig/.view/layout"
# A pane started inside a pane: on Linux the outer kill ends it too and its
# folder goes with it, with no further command. macOS has no subreaper, so
# there the inner pane lives on and is ended through its own ctl.
fux3 start outer -- sh -c 'fux3 start inner -- sh; exec sh' >/dev/null
until_ok test -e "$D/main/inner/screen"
echo kill > "$D/main/outer/ctl"
until_ok test ! -e "$D/main/outer"
if [ "$(uname)" = Linux ]; then
    until_ok test ! -e "$D/main/inner"
else
    [ "$(fux3 ls | sort | tr '\n' ' ')" = "main/inner rig/server " ]
fi
# Killing every pane left never hangs after the kills above.
for c in "$D"/*/*/ctl; do echo kill > "$c"; done
none_left() { [ -z "$(ls "$D/main")$(ls "$D/rig")" ]; }
until_ok none_left
[ -z "$(fux3 ls)" ]
echo "the example ran"
