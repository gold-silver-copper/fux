#!/usr/bin/env bash
# The README's example, run for real: what CI does on every system. Run from
# the repository's root, after building fux2 and fux2-view.
set -euo pipefail
PATH="${FUX2_BIN:-$PWD/target/debug}:$PATH"
export FUX2_DIR
FUX2_DIR=$(mktemp -d)
# A native path: Windows' programs do not read an MSYS one.
if command -v cygpath >/dev/null; then FUX2_DIR=$(cygpath -m "$FUX2_DIR"); fi
chmod 700 "$FUX2_DIR"
cleanup() {
    for pane in $(fux2 ls 2>/dev/null | cut -f1); do fux2 kill "$pane" || true; done
}
trap cleanup EXIT

fux2 start --name build -- sh
token=$(fux2 send build 'echo hi\n')
fux2 read build --since "$token" --wait 20000 --until-line hi --text | grep -qx hi
fux2 read build | grep -qx hi
[ "$(fux2 ls)" = build ]

# A program that is gone before anyone asks still shows its last screen.
fux2 start --name short -- sh -c 'echo last words; exit 3'
for _ in $(seq 100); do
    fux2 ls --long | grep -q '^short.*exited (exit 3)' && break
    sleep 0.1
done
fux2 ls --long | grep -q '^short.*exited (exit 3)'
fux2 read short | grep -qx 'last words'
fux2 kill short

fux2 kill build
[ -z "$(fux2 ls)" ]
echo "the example ran"
