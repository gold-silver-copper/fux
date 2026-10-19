#!/usr/bin/env bash
# release/ucs-detect.sh OUT [NAME [MIN]]: ucs-detect 2.3.8 (wcwidth 0.9.2)
# in a pane of a private fux server, FUX's build, with Ghostty's pinned
# limits (its test/ucs-detect.sh: 5,000 code points and graphemes a
# category, 1,000 errors). Writes OUT/ucs-detect-NAME.json (NAME: fux) and
# its summary, OUT/ucs-detect-NAME.txt: each category's score beside the
# authority that decides it, and the languages that fail. Exit 1 if fewer
# than MIN languages (default 81, as many as pass at release) pass, or
# ucs-detect measured other than its 85. About 5 minutes.
#
# ucs-detect is installed user-locally, never system-wide: UCS_DETECT_PY
# names a folder made by `pip install --target DIR ucs-detect==2.3.8`.
set -euo pipefail
out=${1:?usage: ucs-detect.sh OUT [NAME [MIN]]} name=${2:-fux} min=${3:-81}
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
trap stop EXIT
py=${UCS_DETECT_PY:?set UCS_DETECT_PY to a pip --target folder holding ucs-detect 2.3.8}
for dist in ucs_detect-2.3.8 wcwidth-0.9.2; do
  [[ -d $py/$dist.dist-info ]] || { echo "ucs-detect.sh: $py lacks $dist" >&2; exit 2; }
done
mkdir -p "$out"
out=$(cd "$out" && pwd)
json=$out/ucs-detect-$name.json
rm -f "$json"
fux_server
# -I: the folder's own modules only, nothing from the current directory.
fux_run 80 24 1200 "env LANG=C.UTF-8 python3 -I -c 'import sys; sys.path.insert(0, \"$py\"); sys.argv = [\"ucs-detect\"] + sys.argv[1:]; import ucs_detect; ucs_detect.main()' --limit-codepoints=5000 --limit-graphemes=5000 --limit-errors=1000 --stream=stderr --no-final-summary --set-software-name $name --set-software-version release --save-json $json" || true
[[ -s $json ]] || { echo "ucs-detect.sh: ucs-detect wrote no results" >&2; exit 1; }
python3 -I "$here/ucs-summary.py" "$json" "$min" | tee "$out/ucs-detect-$name.txt"
