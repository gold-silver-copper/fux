# Independent coverage-guided target (macOS)

This excluded workspace has its own lockfile. Production fux-vt depends only
on unicode-width; libfuzzer-sys belongs only to this package.

Verified tooling: cargo-fuzz 0.13.2, `rustc +nightly --version` =
`rustc 1.100.0-nightly (bba531001 2026-09-20)`, Darwin arm64. From repository root:

```sh
FUX_VT_FUZZ_CORPUS="$PWD/fux-vt/fuzz/corpus/terminal" cargo test -p fux-vt --test invariants --locked
cargo fmt --manifest-path fux-vt/fuzz/Cargo.toml --all --check
cargo +nightly fuzz build terminal --fuzz-dir fux-vt/fuzz
cargo +nightly fuzz run terminal --fuzz-dir fux-vt/fuzz -- -max_total_time=600 -max_len=4096 -rss_limit_mb=1024
```

Input: three header bytes choose 1–16 rows, 1–24 columns and 0–15 history
rows. `ff rows cols` resizes; `fe` followed by eight bytes selects window
offset/height/width, both copy endpoints and cell/byte budgets; other opcodes
introduce 1–254 terminal bytes. Processing each operation whole and one byte
at a time must agree on full retained cell/style/wrap data, cursor, history,
replies and modes. Each boundary checks stable-ID uniqueness, all wide halves,
combining capacity, cursor/margins, window clipping and read-only marks.
Copy endpoints can deliberately be invalid; successful results must respect
the supplied budget and match the independently chunked parser.

The 120 adversarial seeds retain generator seeds 0–19 across six input
geometries (the small-dimension fuzz policy reduces the larger geometries).
Thirteen additional seeds cover all eleven permanent golden operation
families, terminal-edge streams, and tiny wrapping/wide glyphs; each includes
an explicit resize/copy operation. Inputs are truncated to 4096 bytes for
this target; the full 160 KiB deterministic stream remains in the ordinary
invariant suite. Coverage-growth inputs are local/ignored, not silently
substituted for the named permanent corpus.

The final evidence ledger records the exact clean-run duration, execution
count, peak RSS, exit status and log path. After any target or production fix,
restart that 600-second clean run; preserve failures as named regression
fixtures. A clean run is not exhaustive correctness proof.
