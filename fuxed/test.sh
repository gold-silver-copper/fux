#!/bin/bash
# test.sh FUXTH: runs fuxed headless on each case in tests/, through the
# fuxth binary FUXTH. A case is a directory: in.txt, the file it opens (none
# for one that doesn't exist yet), named notes.txt, or what name says if
# there; keys, its keys in printf's %b notation, line ends ignored; out.txt, if there, the file as saved, byte for byte;
# mode, if there, the file's mode before, which the save must keep; beside/,
# if there, files put beside it first, which must stay as they were; setup,
# if there, shell sourced in the file's directory just before fuxed runs;
# check, if there, shell run there afterwards by bash -e, a test a line,
# which must succeed. fuxed must quit by itself (exit 0) and, unless check
# says otherwise, leave no other file.
set -u
fuxth=${1:?usage: test.sh FUXTH}
# A path relative to where the caller is, before any cd.
case $fuxth in */*) [ "${fuxth#/}" = "$fuxth" ] && fuxth=$PWD/$fuxth ;; esac
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d) || exit 1
trap 'rm -rf "$work"' EXIT
# A word that runs the rest, so an empty array never expands under set -u
# (bash before 4.4 calls it unbound): `timeout` where there is one.
limit=(command)
command -v timeout >/dev/null && limit=(timeout 10)
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
export -f mode
failed=0
for case in "$here"/tests/*/; do
  name=$(basename "$case")
  dir=$work/$name
  mkdir "$dir"
  file=notes.txt
  [ -f "$case/name" ] && file=$(cat "$case/name")
  [ -f "$case/in.txt" ] && cp "$case/in.txt" "$dir/$file"
  [ -f "$case/mode" ] && chmod "$(cat "$case/mode")" "$dir/$file"
  [ -d "$case/beside" ] && cp -R "$case/beside/." "$dir/"
  printf '%b' "$(tr -d '\n' < "$case/keys")" > "$work/$name.keys"
  (cd "$dir" && { [ ! -f "$case/setup" ] || . "$case/setup"; } &&
    LINES=10 COLUMNS=40 "${limit[@]}" "$fuxth" "$here/fuxed.fux" "$file" \
      < "$work/$name.keys" > /dev/null 2> "$work/$name.err")
  status=$?
  why=
  if [ "$status" -ne 0 ]; then why="exit $status: $(cat "$work/$name.err")"
  elif [ -f "$case/out.txt" ] && ! cmp -s "$dir/$file" "$case/out.txt"; then
    why="saved file differs: $(od -c "$dir/$file" | head -5)"
  elif [ -f "$case/check" ]; then
    (cd "$dir" && bash -e "$case/check") || why="check failed: $(ls -lA "$dir" | tail -n +2 | tr '\n' ';')"
  elif [ "$(ls -A "$dir")" != "$( (echo "$file"; ls -A "$case/beside" 2>/dev/null) | sort)" ]; then
    why="files left: $(ls -A "$dir" | tr '\n' ' ')"
  fi
  if [ -z "$why" ] && [ -d "$case/beside" ] && ! diff -r "$case/beside" "$dir" -x "$file" > /dev/null; then
    why="a file beside it changed"
  elif [ -z "$why" ] && [ -f "$case/mode" ] && [ "$(mode "$dir/$file")" != "$(cat "$case/mode")" ]; then
    why="mode $(mode "$dir/$file"), not $(cat "$case/mode")"
  fi
  if [ -n "$why" ]; then
    echo "FAIL $name: $why"
    failed=1
  else
    echo "ok   $name"
  fi
done
exit $failed
