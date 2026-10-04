# fux-vt fuzz targets

Coverage-guided targets for fux-vt, in a package of their own with its own
lockfile, so libfuzzer-sys never becomes a dependency of fux-vt. They need
a nightly toolchain and cargo-fuzz (CI uses 0.13.2). From the repository
root:

```sh
cargo +nightly fuzz build --fuzz-dir fux-vt/fuzz
cargo +nightly fuzz run terminal --fuzz-dir fux-vt/fuzz -- -max_total_time=600 -max_len=4096 \
  -dict=fux-vt/fuzz/terminal.dict -use_value_profile=1 -rss_limit_mb=4096 -malloc_limit_mb=1024
cargo +nightly fuzz run graphemes --fuzz-dir fux-vt/fuzz -- -max_total_time=120 -use_value_profile=1
cargo +nightly fuzz run cells --fuzz-dir fux-vt/fuzz -- -max_total_time=120 -use_value_profile=1
```

`fux-vt/compare/run.sh fuzz [MINUTES]` runs every target of the repository
in turn, these three included, and minimizes any crash. CI
(`.github/workflows/fuzz.yml`) replays the `terminal` corpus nightly and
runs the other two for a minute each.

Under AddressSanitizer on macOS a run passes 1 GiB of RSS within a minute,
so RSS is capped at 4 GiB and single allocations at 1 GiB instead.
`-use_value_profile=1` rewards progress through comparisons (final bytes,
table lookups) that edge coverage misses.

## Targets

| Target | Input | Checked against |
| --- | --- | --- |
| `terminal` | Raw bytes: a header, then operations (below) | Whole, byte-at-a-time and chunked processing agree; invariants and row versions after every step |
| `graphemes` | Structured (`arbitrary`): lines of characters from `tests/corpus/graphemes.rs` or any scalar value, SGR between them, piece sizes, a width to reflow to | Every row holds the cells `tests/corpus/models.rs` makes of its line (UAX #29 by unicode-segmentation, widths by unicode-width), the cursor after the last; again after reflowing narrower and back |
| `cells` | Structured: a run's length and edits of every kind to `Cells` | The plain list of cells in `tests/corpus/models.rs`; the text budget; exact copies |

The structured targets share their models with `tests/properties.rs` and
reach clustering, spilling and compaction in seconds; `terminal` covers
the parser and everything else.

### `terminal`'s input

Three header bytes, r, c and h, give 1 + r mod 16 rows, 1 + c mod 24
columns and h mod 16 history rows, and their high bits turn options on:

| Byte | 0x10 | 0x20 | 0x40 | 0x80 |
| --- | --- | --- | --- | --- |
| history | `events` | `extended_replies` | `hyperlinks` | `prompt_marks` |
| rows | `reflow` | `kitty_keyboard` | `identity` | `color_scheme_updates` |
| columns | | | `setting_reports` | `rectangle_checksums` |

Then operations:

- `ff r c`: resize (resizing to the same size must change no mark);
- `fe` and eight bytes: a window (offset, height, width), two copy
  endpoints and a cell budget (the byte budget is four times it); the
  endpoints may be invalid, and a copy must respect its budget;
- `fd n` and n mod 64 bytes: a character of `tests/corpus/graphemes.rs`
  for each byte, so long clusters of every kind are common;
- any other byte `k`: the next k + 1 bytes as terminal output.

Each piece of output is processed whole, a byte at a time, and in chunks
of 2 to 17 bytes the output chooses; all three must agree on every cell,
style, wrap, cursor, history row, reply, event and mode. After each step:
row identities are unique, wide glyphs whole, clusters and row text within
their bounds, rows copy into `Cells` unchanged, cursor and margins on the
screen, windows clipped and marks read-only. Row versions are checked
after every byte of the byte-at-a-time copy, against a hash of each row.

`terminal.dict` (also used by `diff/fuzz`'s `oracle`) holds tokens
mutation rarely assembles: cluster material (joiners, selectors, emoji,
flags, tags, Indic, Hangul, Prepend) and fux-vt's sequences.

## Corpus

`corpus/terminal` holds the permanent seeds, cut to 4,096 bytes:
`adversarial-*`, generator seeds 0–19 at the six sizes of
`tests/invariants.rs` (the header's modulus shrinks the larger ones); and
`fixture-*`, each ending with a resize and a copy, for the golden fixtures,
the terminal-edge streams, tiny grids, a history copy, long clusters
(narrow, wide and through reflows), the `fd` operation, the keyboard
protocol with an identity, the newer SGR and cursor sequences, and a row's
text budget. The invariant tests write them:

```sh
FUX_VT_FUZZ_CORPUS="$PWD/fux-vt/fuzz/corpus/terminal" cargo test -p fux-vt --test invariants --locked
```

What a run adds, and the `graphemes` and `cells` corpora, stay local (git
ignores them); a crash becomes a named regression test.

Coverage, as text:

```sh
cargo +nightly fuzz coverage terminal --fuzz-dir fux-vt/fuzz
cargo +nightly cov -- report fux-vt/fuzz/target/*/coverage/*/release/terminal \
  -instr-profile=fux-vt/fuzz/coverage/terminal/coverage.profdata \
  --ignore-filename-regex='(registry|rustc|fuzz_targets)'
```
