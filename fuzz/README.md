# Coverage-guided targets for fux (macOS)

This excluded workspace has its own lockfile. It depends only on
libfuzzer-sys, fux and fux-vt by path; fux's own dependencies are unchanged.
fux-vt's parser has its own target in `fux-vt/fuzz`.

Verified tooling: cargo-fuzz 0.13.2, `rustc +nightly --version` =
`rustc 1.100.0-nightly (bba531001 2026-09-20)`, Darwin arm64. From repository
root:

```sh
cargo fmt --manifest-path fuzz/Cargo.toml --all --check
cargo +nightly fuzz build --fuzz-dir fuzz
cargo +nightly fuzz run TARGET --fuzz-dir fuzz -- -max_total_time=600 -max_len=4096 -rss_limit_mb=1024 -timeout=25
```

`TARGET` is one of `protocol`, `keys`, `paint` or `layout`. `-timeout=25`
makes a hang a failure within the run; libFuzzer's own default is 1200 s,
longer than the run.

## Targets

**`protocol`**: bytes from a peer on the server's socket, into
`protocol::Decoder`.
- Input: one byte choosing a piece size (0 is the whole stream at once), then
  the stream.
- Each target checks:
  - nothing panics;
  - after the frames are drained, `buffered()` is under `4 + MAX_FRAME`. All
    it holds is one incomplete frame, whose header claimed at most `MAX_FRAME`;
  - the frames and the first error are the same pushed whole, in pieces, and
    byte by byte;
  - each decoded frame, re-encoded with `Frame::encode`, decodes back to itself.

**`keys`**: an attached client's terminal bytes, into `decode::Decoder`, with
the Escape deadline passing only at the end.
- Input: one byte choosing a piece size, then the stream.
- The inputs are the same whole, in pieces, and byte by byte.
- No paste text is longer than `PASTE_LIMIT` chars. The limit counts pasted
  bytes, and invalid UTF-8 becomes U+FFFD, three bytes, so the text is bounded
  in chars, one per byte, not in bytes.

**`paint`**: `render::paint` diffs, applied to a fux-vt terminal.
- Input: two bytes for the size, 1–40 rows by 1–120 columns, where `ff ff` is
  one row of `u16::MAX` columns. Then a flags byte (bits 0 and 1: old and new
  cursors; bit 2: a new size of its own, which forces a full repaint), the
  cursors, and four-byte cell writes.
- Glyphs are ASCII, a blank, `é`, `e` with a combining accent, and `界`.
  Attribute sets never hold both bold and dim, as fux never paints both and
  fux-vt keeps them apart (SGR 1 and 2 replace one another). Wide glyphs are
  kept whole.
- The terminal is painted `old` from nothing, then the paint from `old` to
  `new`. Every cell must then equal `new`'s in text, width and attributes,
  where a blank reads as a space and a wide glyph in the last column is
  painted as a blank. The cursor must be `new`'s, or hidden.

**`layout`**: `layout::place` on normalized trees, then `resize`.
- Input: two bytes for the area's origin, four for its width and height (a high
  byte below `0x80` is a small size, `lo % 64`; `ff` is `u16::MAX`), a tree of
  up to 16 panes four deep with weights from 0 to `u32::MAX`, then two-byte
  resizes.
- After every placement:
  - every pane is in the tree, placed once, not empty, and at least `MIN` in
    each dimension where the area has room;
  - panes and separators lie inside the area and never overlap;
  - when the whole tree fits (the area is at least the tree's minimum, computed
    by the rule `min_len` documents), they tile it exactly. Where it does not
    fit, a split without room for its first child shows nothing, by design;
  - `neighbor` only names another placed pane.

## Corpus

`corpus/TARGET/fixture-*` are the permanent seeds, taken from the unit tests
that state each property:
- `protocol`: every frame of `every_frame_round_trips_in_any_chunking`, encoded
  by fux, and the malformed frames of `malformed_frames_are_errors_not_panics`;
- `keys`: `every_key_sequence_decodes`,
  `a_sequence_split_at_every_byte_decodes_the_same`, and the Escape, mouse and
  paste cases;
- `paint`: `a_diff_applied_to_the_old_grid_gives_the_new_one`,
  `painting_the_widest_last_column_ends`, a resize and combining marks;
- `layout`: the split, nested, small-area and resize tests, and extremes.

`corpus/TARGET/regression-*` are minimized inputs of fixed findings. Coverage
growth stays local and ignored.
