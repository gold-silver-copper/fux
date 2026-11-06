#!/bin/sh
# Records nvim drawing a buffer into demo/nvim.typescript, killed while it
# shows (so the screen is left as drawn), then checks demo/nvim.assert.
set -e
cd "$(dirname "$0")"
script -q -c "stty rows 24 cols 80; TERM=xterm-256color timeout -s KILL 1.5 nvim --clean \
  -c 'set title titlestring=oracle-demo' -c 'call setline(1, [\"hello\", \"wide: 世界\"])' \
  -c 'call cursor(2, 7)'" nvim.typescript >/dev/null || true
cargo run -q --release -- --script "$@" nvim.typescript nvim.assert
