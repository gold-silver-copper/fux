#!/bin/sh
# Drives the mux under `script` as a person would: nvim on the left, top on
# the right; edits in nvim, quits it, moves to top, quits it.
sleep 1.5
printf 'ihello from nvim\033'; sleep 0.5
printf ':wq! %s/note.txt\r' "$TMPDIR"; sleep 0.8
printf '\001o'; sleep 0.8
printf 'q'; sleep 1
