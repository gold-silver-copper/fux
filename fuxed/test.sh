#!/bin/bash
# test.sh FUXTH: runs fuxed headless on each case in tests/, through the
# fuxth binary FUXTH. A case is a directory: in.txt, the file it opens (none
# for one that doesn't exist yet); keys, its keys in printf's %b notation,
# line ends ignored; out.txt, the file as saved, byte for byte; and mode, if
# there, the file's mode before, which the save must keep; beside/, if there,
# files put beside it first, which must stay as they were. fuxed must quit
# by itself (exit 0) and leave no other file.
set -u
fuxth=${1:?usage: test.sh FUXTH}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
limit=()
command -v timeout >/dev/null && limit=(timeout 10)
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
failed=0
for case in "$here"/tests/*/; do
  name=$(basename "$case")
  dir=$work/$name
  mkdir "$dir"
  [ -f "$case/in.txt" ] && cp "$case/in.txt" "$dir/notes.txt"
  [ -d "$case/beside" ] && cp -R "$case/beside/." "$dir/"
  [ -f "$case/mode" ] && chmod "$(cat "$case/mode")" "$dir/notes.txt"
  printf '%b' "$(tr -d '\n' < "$case/keys")" > "$work/$name.keys"
  (cd "$dir" && LINES=10 COLUMNS=40 "${limit[@]}" "$fuxth" "$here/fuxed.fux" notes.txt \
    < "$work/$name.keys" > /dev/null 2> "$work/$name.err")
  status=$?
  why=
  if [ "$status" -ne 0 ]; then why="exit $status: $(cat "$work/$name.err")"
  elif ! cmp -s "$dir/notes.txt" "$case/out.txt"; then why="saved file differs: $(od -c "$dir/notes.txt" | head -5)"
  elif [ "$(ls -A "$dir")" != "$( (echo notes.txt; ls -A "$case/beside" 2>/dev/null) | sort)" ]; then
    why="files left: $(ls -A "$dir" | tr '\n' ' ')"
  elif [ -d "$case/beside" ] && ! diff -r "$case/beside" "$dir" -x notes.txt > /dev/null; then
    why="a file beside it changed"
  elif [ -f "$case/mode" ] && [ "$(mode "$dir/notes.txt")" != "$(cat "$case/mode")" ]; then
    why="mode $(mode "$dir/notes.txt"), not $(cat "$case/mode")"
  fi
  if [ -n "$why" ]; then
    echo "FAIL $name: $why"
    failed=1
  else
    echo "ok   $name"
  fi
done
exit $failed
