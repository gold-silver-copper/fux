# Independent coverage-guided targets (macOS)

This excluded workspace has its own lockfile. Production fux-vt depends only
on unicode-width; libfuzzer-sys belongs only to this package.

Verified tooling: cargo-fuzz 0.13.2, `rustc +nightly --version` =
`rustc 1.100.0-nightly (bba531001 2026-09-20)`, Darwin arm64. From repository root:

```sh
FUX_VT_FUZZ_CORPUS="$PWD/fux-vt/fuzz/corpus/terminal" cargo test -p fux-vt --test invariants --locked
cargo fmt --manifest-path fux-vt/fuzz/Cargo.toml --all --check
cargo +nightly fuzz build --fuzz-dir fux-vt/fuzz
cargo +nightly fuzz run terminal --fuzz-dir fux-vt/fuzz -- -max_total_time=600 -max_len=4096 \
  -dict=fux-vt/fuzz/terminal.dict -use_value_profile=1 -rss_limit_mb=4096 -malloc_limit_mb=1024
cargo +nightly fuzz run graphemes --fuzz-dir fux-vt/fuzz -- -max_total_time=120 -use_value_profile=1
cargo +nightly fuzz run cells --fuzz-dir fux-vt/fuzz -- -max_total_time=120 -use_value_profile=1
```

## Targets

| Target | Input | Oracle | Speed |
| --- | --- | --- | --- |
| `terminal` | Raw bytes: a header and operations (below) | Whole, byte-at-a-time and chunked processing agree; invariants and row versions after every step | Tens of runs a second: every byte of the byte-at-a-time copy is checked |
| `graphemes` | Structured (`arbitrary`): lines of characters from `tests/corpus/graphemes.rs` or any scalar value, SGR between them, piece sizes, a width to reflow to | Every row holds exactly the cells `tests/corpus/models.rs` makes of its line (UAX #29 by unicode-segmentation, widths by unicode-width), the cursor after the last; again after reflowing narrower and back | Thousands of runs a second |
| `cells` | Structured: a run's length and edits of every kind to `Cells` | The plain list of cells in `tests/corpus/models.rs`, which `tests/properties.rs` also checks against; the text budget; exact copies | Thousands of runs a second |

The two structured targets cannot waste runs on input that does not parse,
and check results against the same models as the property tests, so they
reach deep into clustering, spilling and compaction in seconds. `terminal`
stays the target for the parser and everything else, with the strongest
general oracles. `-use_value_profile=1` rewards progress through
comparisons (sequence final bytes, table lookups) that edge coverage alone
does not see; in two minutes it took `terminal` past the coverage a
ten-minute run reached without it. The structured targets' corpora are
local (`corpus/graphemes`, `corpus/cells` are ignored).

Under AddressSanitizer on macOS the process passes 1 GiB of RSS within a
minute even on 0.1.5, so RSS is capped at 4 GiB and any single allocation at
1 GiB instead: a runaway allocation is still caught.

`terminal.dict` gives the fuzzer tokens random mutation rarely assembles:
joiners, selectors, emoji and modifiers, regional indicators, tag
characters, Indic consonants, viramas and vowel signs, Hangul jamo, a
Prepend and a Control-class format character, and the SGR, cursor, keyboard
protocol and identity sequences fux-vt added.

Coverage of the permanent corpus and whatever a run added, as text:

```sh
cargo +nightly fuzz coverage terminal --fuzz-dir fux-vt/fuzz
cargo +nightly cov -- report fux-vt/fuzz/target/*/coverage/*/release/terminal \
  -instr-profile=fux-vt/fuzz/coverage/terminal/coverage.profdata \
  --ignore-filename-regex='(registry|rustc|fuzz_targets)'
```

Input: three header bytes choose 1–16 rows, 1–24 columns and 0–15 history
rows. The history byte's bits 0x10 and 0x20 turn on `Options::events` and
`extended_replies`; the rows byte's bits 0x10, 0x20, 0x40 and 0x80 turn on
`reflow`, `kitty_keyboard`, an `identity` and `color_scheme_updates`, each
alone or together.
`ff rows cols` resizes; `fe` followed by eight bytes selects window
offset/height/width, both copy endpoints and cell/byte budgets; `fd n`
followed by n (at most 63) bytes prints a character of
`tests/corpus/graphemes.rs` for each, so clusters of every kind, and longer
than a cell holds inline, are common; other opcodes introduce 1–253
terminal bytes. Processing each operation whole, one byte at a time, and
in chunks of 2 to 17 bytes chosen by the operation's bytes (so runs of text
and clusters break where neither of the others breaks them) must agree on full retained cell/style/wrap data, cursor, history,
replies and modes. Each boundary checks stable-ID uniqueness, all wide halves,
cluster capacity, every row's text within its budget, every cell with
contents showing them, rows copying into `Cells` unchanged, cursor/margins,
window clipping and read-only marks; resizing to the size a screen
already has changes no mark. Row versions are checked after every byte of
the byte-at-a-time copy against a hash of each row, not a copy of it:
under instrumentation, copying rows and a hash map made the check nine
tenths of the target's time.
Copy endpoints can deliberately be invalid; successful results must respect
the supplied budget and match the independently chunked parser.

The 120 adversarial seeds retain generator seeds 0–19 across six input
geometries (the small-dimension fuzz policy reduces the larger geometries).
Twenty-two additional seeds cover all eleven permanent golden operation
families, terminal-edge streams, tiny wrapping/wide glyphs, the widened
history-copy regression, long grapheme clusters (in a 12-column and a
2-column grid, and with reflow through several resizes), the grapheme
operation, the kitty keyboard protocol with an identity, and the new SGR
and cursor sequences, and a row's text budget (exhausted, compacted and
cut to fit a narrower row, with and without reflow); each includes an
explicit resize/copy operation. Inputs are truncated to 4096 bytes for
this target; the full 160 KiB deterministic stream remains in the ordinary
invariant suite. Coverage-growth inputs are local/ignored, not silently
substituted for the named permanent corpus.

The final evidence ledger records the exact clean-run duration, execution
count, peak RSS, exit status and log path. After any target or production fix,
restart that 600-second clean run; preserve failures as named regression
fixtures. A clean run is not exhaustive correctness proof.
