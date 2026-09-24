#!/bin/sh
# Run every repro script and compare its verdicts with expected.tsv.
#
# Usage: fux-fuzz/repro/check.sh /path/to/fux
# Exit 0 when every script gives the expected verdict three ways (see
# expected.tsv), 1 when any differs, 2 on a setup problem. Prints one line per
# script, and the output of any script that surprised it.
#
# Some findings depend on more than the platform: 014 needs a filesystem that
# reuses inode numbers (ext4), which a container's overlayfs is not. A script
# that cannot show its finding here says so on a line starting NOT-APPLICABLE:
# and exits 2; that is reported as "n/a here" rather than a mismatch. With
# FUX_REPRO_STRICT=1, as CI sets on runners whose /tmp is ext4, it is a
# mismatch, so the table's verdict is still required where it can be shown.
set -u
FUX="${1:?usage: $0 /path/to/fux}"
[ -x "$FUX" ] || { echo "not executable: $FUX" >&2; exit 2; }
case "$FUX" in /*) ;; *) FUX="$PWD/$FUX" ;; esac
HERE="$(CDPATH="" cd -- "$(dirname -- "$0")" && pwd)"
case "$(uname -s)" in
  Linux) COLUMN=2 ;;
  Darwin) COLUMN=3 ;;
  *) echo "no expectations for $(uname -s)" >&2; exit 2 ;;
esac
OUT="$(mktemp -d)" || exit 2
trap 'rm -rf "$OUT"' EXIT INT TERM

failed=0
seen=0
for script in "$HERE"/[0-9][0-9][0-9]-*.sh; do
  number="$(basename "$script" | cut -c1-3)"
  expected="$(awk -v n="$number" -v c="$COLUMN" '!/^#/ && $1 == n { print $c }' "$HERE/expected.tsv")"
  if [ -z "$expected" ]; then
    echo "$number: no expectation in expected.tsv"
    failed=1
    continue
  fi
  seen=$((seen + 1))
  control_expected=1
  [ "$expected" = 2 ] && control_expected=2

  sh "$script" "$FUX" > "$OUT/reproduce" 2>&1
  reproduce=$?
  NEGATIVE_CONTROL=1 sh "$script" "$FUX" > "$OUT/control" 2>&1
  control=$?
  sh "$script" /nonexistent/fux > "$OUT/bad" 2>&1
  bad=$?

  if [ "$reproduce" = "$expected" ] && [ "$control" = "$control_expected" ] && [ "$bad" = 2 ]; then
    echo "$number: ok ($reproduce/$control/$bad)"
  elif [ "$reproduce" = 2 ] && [ "${FUX_REPRO_STRICT:-0}" != 1 ] && [ "$bad" = 2 ] &&
    reason="$(grep -m1 '^NOT-APPLICABLE:' "$OUT/reproduce")"; then
    echo "$number: n/a here (${reason#NOT-APPLICABLE: }), wanted $expected where it applies"
  else
    echo "$number: UNEXPECTED $reproduce/$control/$bad, wanted $expected/$control_expected/2"
    for part in reproduce control bad; do
      echo "  --- $part"
      tail -15 "$OUT/$part" | sed 's/^/  /'
    done
    failed=1
  fi
done
[ "$seen" -gt 0 ] || { echo "no repro scripts found" >&2; exit 2; }
exit "$failed"
