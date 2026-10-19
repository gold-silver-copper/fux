#!/usr/bin/env bash
# release/vttest.sh ENGINE OUT: vttest 2.7 (20251205), live at 24x80.80, in
# ENGINE: `fux` (a pane of a private fux server, FUX's build: fux-vt as fux
# runs it) or `xterm` (real xterm 411, its own defaults, on a private Xvfb
# display). vttest replays vttest.keys as its own command file (-c), reads
# every answer from the terminal live, and logs (-l) each self-check;
# vttest.py counts them. Writes OUT/vttest-ENGINE.log and .txt. For fux,
# exit 1 unless the self-checks are as fux-vt/compare/vttest-expected.txt
# lists them; for xterm, unless the walk ran whole. About 5 minutes each
# (vttest waits a second after each menu choice it replays).
#
# vttest is built from references/xterm/vttest.tar.gz (references/fetch.sh
# fetches it; VTTEST_TGZ names another copy), checked by its SHA-256.
set -euo pipefail
engine=${1:?usage: vttest.sh fux|xterm OUT} out=${2:?usage: vttest.sh fux|xterm OUT}
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
trap stop EXIT
mkdir -p "$out"
out=$(cd "$out" && pwd)

tgz=${VTTEST_TGZ:-$repo/references/xterm/vttest.tar.gz}
sha=cd6886f9aefe6a3f6c566fa61271a55710901a71849c630bf5376aa984bf77cc
[[ $(sha256sum "$tgz" | cut -d' ' -f1) == "$sha" ]] || { echo "vttest.sh: $tgz is not vttest 20251205" >&2; exit 2; }
mkdir "$work/src"
tar -xzf "$tgz" -C "$work/src" --strip-components=1
(cd "$work/src" && ./configure -q >/dev/null && make -s -j2 >/dev/null 2>&1)
vt=$work/src/vttest
log=$out/vttest-$engine.log

# One run at operating level LEVEL's walk.
run() {
  python3 -I "$here/vttest.py" cmds "$here/vttest.keys" "$1" "$work/cmds"
  rm -f "$log"
  local cmd="$vt -c $work/cmds -l $log 24x80.80"
  case $engine in
    fux) fux_run 80 24 900 "$cmd" || true ;;
    xterm) timeout 900 xterm -geometry 80x24 -ut -e sh -c "$cmd" || true ;;
  esac
}

case $engine in
  fux) fux_server ;;
  xterm) xvfb_start ;;
  *) echo "vttest.sh: ENGINE is fux or xterm" >&2; exit 2 ;;
esac
# The walk differs below level 3 and from it (vttest.keys): guess by the
# engine (fux-vt's DA claims level 2, xterm 411's VT420 level 4), and run
# again if the log's level says otherwise.
guess=2
[[ $engine == xterm ]] && guess=4
run "$guess"
level=$(sed -n 's/^Note: Cur Operating Level: //p' "$log" | head -n1)
if [[ -n $level ]] && (( (level >= 3) != (guess >= 3) )); then
  run "$level"
fi
status=0
python3 -I "$here/vttest.py" results "$log" > "$out/vttest-$engine.txt" || status=$?
tail -n1 "$out/vttest-$engine.txt"
if [[ $engine == fux ]]; then
  python3 -I "$here/vttest.py" check "$log" "$here/../vttest-expected.txt" fux-vt || status=$?
fi
exit "$status"
