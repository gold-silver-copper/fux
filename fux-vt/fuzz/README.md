# fux-vt fuzz targets

Coverage-guided targets for fux-vt, in a package of their own with its own
lockfile, so libfuzzer-sys never becomes a dependency of fux-vt. They need
a nightly toolchain and cargo-fuzz (CI uses 0.13.2). From the repository
root:

```sh
cargo +nightly fuzz build --fuzz-dir fux-vt/fuzz -O -a
cargo +nightly fuzz run TARGET --fuzz-dir fux-vt/fuzz -O -a fux-vt/fuzz/corpus/TARGET fux-vt/fuzz/seeds/TARGET -- \
  -max_total_time=600 -rss_limit_mb=2048 -timeout=25 OPTIONS
```

Each target's OPTIONS (its dictionary, `-max_len`, value profiling, its
own timeout) are its line of `fux-vt/compare/fuzz-options.txt`, which
`run.sh fuzz` and CI both read; a corpus or seeds directory that does not
exist is left out.

`fux-vt/compare/run.sh fuzz [MINUTES]` runs every target of the repository
in turn, these six included, from their corpus and seeds, and minimizes
any crash. CI
(`.github/workflows/fuzz.yml`) explores each of them nightly for five
minutes, a job each, from its corpus, its frozen seeds and what earlier
nights found, which the Actions cache keeps between nights.

Under AddressSanitizer on macOS a run passes 1 GiB of RSS within a minute;
there, give it `-rss_limit_mb=4096 -malloc_limit_mb=1024` after the
options instead. `-use_value_profile=1` rewards progress through comparisons (final bytes,
table lookups) that edge coverage misses.

## Targets

| Target | Input | Checked against |
| --- | --- | --- |
| `terminal` | Raw bytes: a header, then operations (below) | The fast target: whole, byte-at-a-time and chunked processing agree, fux-vt's invariant checker and row versions after every operation |
| `deep` | Raw bytes: six header bytes, then operations (`fuzz_targets/deep.rs` lists them) | Large state: up to 511 by 511 cells, 8 MiB of history, every `Feature`, sixel drawn in a host cell the input chooses, up to 1 MiB of output a run in 4 KiB reads by `process_until_stop`, each call within the frozen ceiling of work (`threats.md` 4.4); every host call (resize, history budgets, prints, palette, nonces, pictures listed and encoded within `encode`'s bound, the picture budget, windows); a byte-at-a-time copy with row versions checked after every byte while it holds at most 2,048 rows and until they have hashed 2^27 cells; the checker after every operation |
| `graphemes` | Structured (`arbitrary`): lines of characters from `tests/corpus/graphemes.rs` or any scalar value, SGR between them, piece sizes, a width to reflow to | Every row holds the cells `tests/corpus/models.rs` makes of its line (UAX #29 by unicode-segmentation, widths by unicode-width), the cursor after the last; again after reflowing narrower and back |
| `cells` | Structured: a run's length and edits of every kind to `Cells` | The plain list of cells in `tests/corpus/models.rs`; the text budget; exact copies |
| `keys` | One byte choosing a piece size (0 for the stream whole), then a terminal's input bytes, into `keys::decode::Decoder`, the Escape deadline passing only at the end | The same inputs whole, in pieces and byte by byte; no paste longer than `PASTE_LIMIT` chars (the limit counts bytes, and invalid UTF-8 becomes U+FFFD) |

The structured targets share their models with `tests/properties.rs` and
reach clustering, spilling and compaction in seconds; `terminal` covers
the parser and everything else.

### `terminal`'s input

Three header bytes, r, c and h, give 1 + r mod 16 rows, 1 + c mod 24
columns and h mod 16 history rows, and their high bits turn options on;
c's 0x10 makes the next byte a fourth header byte, f, for the rest:

| Byte | 0x10 | 0x20 | 0x40 | 0x80 |
| --- | --- | --- | --- | --- |
| history | `Events` | `ExtendedReplies` | `Hyperlinks` | `PromptMarks` |
| rows | `Reflow` | `KittyKeyboard` | `identity` | `ColorSchemeUpdates` |
| columns | (f follows) | `Pictures` | `SettingReports` | `RectangleChecksums` |

| Byte | 0x01 | 0x02 | 0x04 | 0x08 |
| --- | --- | --- | --- | --- |
| f | `ModeReports` | `InBandResize` | `SizeReports` | `Palette` |

Then operations:

- `ff r c`: resize (resizing to the same size must change no mark),
  after setting history's budget to (r >> 4) × 64 bytes if r ≥ 0x10;
- `fe` and eight bytes: a window (offset, height, width), two copy
  endpoints and a cell budget (the byte budget is four times it); the
  endpoints may be invalid, and a copy must respect its budget;
- `fd n` and n mod 64 bytes: a character of `tests/corpus/graphemes.rs`
  for each byte, so long clusters of every kind are common;
- any other byte `k`: the next k + 1 bytes as terminal output.

Each piece of output is processed whole, a byte at a time, and in chunks
of 2 to 17 bytes the output chooses, the chunks by `process_until_stop`
given again after each stop, as fux's pane reads, with each frame's end
found by `FrameScan` where the parser ends it. Prints are recorded, and
held within their caps. All three must agree on every cell,
style, wrap, cursor, history row, reply, event and mode. After each step:
fux-vt's invariant checker (`Parser::check`), row identities unique, wide
glyphs whole, clusters and row text within their bounds, rows copy into
`Cells` unchanged, windows clipped and marks read-only; and every row
whose cells, wrap or prompt mark the operation changed has a newer version.
The `deep` target checks versions after every byte.

`terminal.dict` (also used by `deep` and `diff/fuzz`'s `oracle`) holds
tokens mutation rarely assembles: cluster material (joiners, selectors,
emoji, flags, tags, Indic, Hangul, Prepend), fux-vt's sequences, sixel's
(its introducer, raster attributes, colours, repeats, bands, CAN and SUB),
and what the safety reproducers send (frames, reports, requests, OSC 4,
52, 9, 777 and 99, RIS, DECSTR, REP's largest count, DECCRA, DECALN).

## Corpus

`corpus/terminal` holds the permanent seeds, cut to 4,096 bytes:
`adversarial-*`, generator seeds 0–19 at the six sizes of
`tests/invariants.rs` (the header's modulus shrinks the larger ones); and
`fixture-*`, each ending with a resize and a copy, for short fixtures,
the terminal-edge streams, tiny grids, a history copy, long clusters
(narrow, wide and through reflows), the `fd` operation, the keyboard
protocol with an identity, the newer SGR and cursor sequences, a row's
text budget, and every `Feature` on (`fixture-every-feature`, through the
fourth header byte). They are kept as they were written; nothing
regenerates them.

What a run adds, and the `graphemes` and `cells` corpora, stay local (git
ignores them); a crash becomes a named regression test.

`seeds/terminal`, `seeds/deep` and `seeds/keys` hold the frozen seeds:
each safety reproducer of `threats.md` (`b01-raster-pictures`,
`b20-long-osc`, …) in the target's format, written from its definition
in `tests/seeds.rs`: a `terminal` one past that target's 4,096 bytes in
`deep`'s (64 KiB, 128 operations, a repeated pattern as `f2`); `tests/seeds.rs`
also replays each at its row's screen size and checks that it reaches
the limit it is for. A reproducer of host calls is in `deep`'s format with
the host's operations after its output (`b06-clone-mid-picture`,
`b08-encode-colours`) where `deep` makes those calls. A seed that is no
fuzz input (one no run of either holds, one fed after a prefix a run
can't hold or takes too long on, one of host calls no target makes) is on
`tests/seeds.rs`'s `NO_FUZZ_INPUT` with why, which a test holds to; it is
kept as `tests/seeds/*.bin`. They are kept apart from `corpus/`, which is cut to
the targets' lengths: give `seeds/TARGET` to a run as a second corpus
directory, as `fux-vt/compare/harness.sh` does. `seeds/graphemes` holds
the inputs that target found (FA-SB-2), which
`fux-vt/fuzz/tests/graphemes_seeds.rs` reads as the target does and
checks as it checks (`tests/corpus/graphemes_target.rs`, shared with the
target), on every test run.

What the `terminal` and `deep` targets reach, each `Feature` on and each
host call made (`process_until_stop`, `FrameScan`, `set_history_bytes`,
`record_prints`, `resize`, `full_refresh_since`, window text, and deep's
others), on their corpus and frozen seeds: `fux-vt/fuzz/reach.sh` builds
them with the `reach` feature, replays every input, and fails naming each
input that fails its checks and anything not reached.

Coverage, as text:

```sh
cargo +nightly fuzz coverage terminal --fuzz-dir fux-vt/fuzz
cargo +nightly cov -- report fux-vt/fuzz/target/*/coverage/*/release/terminal \
  -instr-profile=fux-vt/fuzz/coverage/terminal/coverage.profdata \
  --ignore-filename-regex='(registry|rustc|fuzz_targets)'
```
