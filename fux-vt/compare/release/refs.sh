#!/usr/bin/env bash
# release/refs.sh OUT [ENGINE...]: alacritty's ref tests (45) and JediTerm's
# test data (24), each case's bytes read by every ENGINE (default: fux and
# xterm) at the case's size, and the screens scored by refs.py: beside the
# suite's own expected screen, and beside real xterm 411's. `fux` is a pane
# of a private fux server (FUX's build), `xterm` real xterm 411 on a private
# Xvfb display, printing its screen (MC), set up as compare's xterm engine
# is where it matters (UTF-8 always, no precomposing) and fed a byte a
# write: fed in larger pieces, xterm 411 shows alacritty/scroll_up_reset's
# line 17 one way or another from run to run, as its reads happen to split
# the bytes. Writes OUT/refs/ (the cases, each
# engine's raw screens, scores.json) and OUT/refs.txt. Exit 1 if fux-vt's
# screen differs from xterm's where release/refs-expected.txt does not say
# why, or agrees where it does. About 3 minutes.
#
# The suites are fetched, pinned, into references/suites/ (REFS_SUITES
# names another folder): alacritty 54214706 and JediTerm 1b354a63, the
# commits the panel's census ran.
set -euo pipefail
out=${1:?usage: refs.sh OUT [ENGINE...]}
shift
engines=("$@")
[[ ${#engines[@]} -eq 0 ]] && engines=(fux xterm)
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
trap stop EXIT
mkdir -p "$out"
out=$(cd "$out" && pwd)
suites=${REFS_SUITES:-$repo/references/suites}

# A suite's PATH at COMMIT, fetched once into DIR: fetch DIR URL COMMIT PATH.
fetch() {
  if [[ $(git -C "$1" rev-parse HEAD 2>/dev/null) == "$3" ]]; then
    return
  fi
  rm -rf "$1"
  git init -q "$1"
  git -C "$1" sparse-checkout set --no-cone "/$4/"
  git -C "$1" fetch -q --depth 1 --filter=blob:none "$2" "$3"
  git -C "$1" -c advice.detachedHead=false checkout -q FETCH_HEAD
}
fetch "$suites/alacritty" https://github.com/alacritty/alacritty \
  542147067dd016fc5f49120ef0ba605a98a4e3a7 alacritty_terminal/tests/ref
fetch "$suites/jediterm" https://github.com/JetBrains/jediterm \
  1b354a63f9845aee3474b0749af0a62e406f0076 core/tests/resources/testData

dir=$out/refs
rm -rf "$dir"
mkdir -p "$dir/raw"
python3 -I "$here/refs.py" cases "$suites/alacritty/alacritty_terminal/tests/ref" \
  "$suites/jediterm/core/tests/resources/testData" "$dir"
# The bytes as they are (no CR added to LF, no echo of any answer), a byte
# a write, for every engine (see xterm, above).
reader="stty raw -echo; dd bs=1 status=none if="

for engine in "${engines[@]}"; do
  mkdir -p "$dir/raw/$engine"
  case $engine in
    fux)
      fux_server
      while read -r name rows cols bytes; do
        fux_run "$cols" "$rows" 120 "sh -c '$reader$bytes'"
        "$fux_bin" "$pane" wrapped capture json > "$dir/raw/fux/$name"
        "$fux_bin" str agent workspace kill >/dev/null
      done < <(python3 -I "$here/refs.py" list "$dir")
      ;;
    xterm)
      xvfb_start
      while read -r name rows cols bytes; do
        printout=$dir/raw/xterm/$name
        timeout 120 xterm -geometry "${cols}x$rows" -ut \
          -xrm 'XTerm*locale: false' -xrm 'XTerm*utf8: 2' -xrm 'XTerm*precompose: false' \
          -xrm "XTerm*printerCommand: cat > $printout" -xrm 'XTerm*printAttributes: 0' \
          -xrm 'XTerm*printerAutoClose: true' -xrm 'XTerm*printerExtent: true' \
          -xrm 'XTerm*printRawChars: true' -xrm 'XTerm*printerNewLine: true' \
          -e sh -c "$reader$bytes; printf '\\033[i'; while [ ! -s $printout ]; do sleep 0.05; done; sleep 0.2" || true
      done < <(python3 -I "$here/refs.py" list "$dir")
      ;;
    *) echo "refs.sh: ENGINE is fux or xterm" >&2; exit 2 ;;
  esac
done
python3 -I "$here/refs.py" score "$dir" "$here/refs-expected.txt" | tee "$out/refs.txt"
exit "${PIPESTATUS[0]}"
