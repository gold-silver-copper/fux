# The oracle: fux-vt beside a commit

An excluded workspace with its own lockfile, for changes meant to leave
fux-vt's behaviour as it was (a refactor, a speed-up, a rewrite). It
forbids the same lints as fux, and its release profile checks arithmetic
for overflow. CI checks that it passes clippy against the commit under test.

`fux-vt-oracle` holds the working tree's fux-vt to what fux-vt does at a
commit, by default the latest `main`. Both get the same bytes, resizes and
API calls, and everything fux-vt's public API shows is compared after
every step. Any difference fails, shrunk to the smallest case that
shows it.

```sh
diff/oracle.sh                       # against the latest origin/master, fetched first
diff/oracle.sh HEAD                  # against another commit
diff/oracle.sh --cases 50000 --seed 7
diff/oracle.sh --replay diff/target/oracle/NAME.case

# The fuzz target, as fux-vt/compare/run.sh fuzz runs it:
cargo +nightly fuzz run oracle --fuzz-dir diff/fuzz -O -a diff/fuzz/corpus/oracle -- \
  -dict=fux-vt/fuzz/terminal.dict -rss_limit_mb=2048 -timeout=25
```

`oracle.sh [REF] [ARGS]` builds and runs `fux-vt-oracle` with `ARGS`:

| Flag | Meaning |
| --- | --- |
| `--seed N` | the seed (default 1) |
| `--cases N` | random cases (default 10000) |
| `--streams N` | resize streams (default 50) |
| `--cells N` | runs of the standalone types (default 300) |
| `--no-corpus` | skip the corpus |
| `--shrink SECONDS` | how long to shrink a difference (default 60) |
| `--threads N` | threads (default: every core) |
| `--replay FILE` | run one written case |
| `--seeds DIR` | write the random cases, resize streams and limits to `DIR` as fuzz seeds, instead of running |
| `--baseline` | print the pinned commit |

`fux-vt/compare/run.sh quick` runs `oracle.sh`; `run.sh fuzz` writes
seeds with `--seeds` and runs the fuzz target.

### The commit

The commit is `base-vt`, a git dependency at a `rev` in
`oracle/Cargo.toml`. Each run pins it to `REF` (by a `file://` URL to this
repository, so any local commit works), builds, and puts
`oracle/Cargo.toml` and `Cargo.lock` back when it exits: a run leaves no
change behind, and the committed `rev` is only a resting value nobody
bumps. `oracle.sh --pin [REF]` pins and leaves the pin in place; CI runs
`oracle.sh --pin HEAD` before it builds this workspace, so the oracle
always builds with both sides at one API. The fuzz target builds against
the resting `rev`; when that no longer builds, run `oracle.sh --pin` first
and `git checkout` the two files after.

Both sides are read by one adapter, `side!` in `oracle/src/side.rs`, with
no per-side code. A branch that changes fux-vt's public API does not build
against `main`, and `oracle.sh` says so in one message: to compare such a
branch, add a temporary shim for the base side in `side.rs`, and never
commit it.

### What is compared

Each side is read into one model (`oracle/src/model.rs`) through fux-vt's
public API (`oracle/src/side.rs`) after every step:

- **The screen's state:** size, cursor, pending wrap, every mode, the key
  encoding, history length, the mark, `resize_report`, the window's size,
  the 256 palette entries and the dynamic colours as the program set them,
  the pictures shown (place, size and a sample of their pixels), the live
  rows of the screen not in use (identity, whether each line goes on, and
  their cells hashed).
- **The host's calls,** as steps between outputs (`Step::Host`): history
  budgets, `clear_history`, and the host's palette and graphics.
- **Every retained row:** identity and version, whether its line goes on
  (`Row::continues`), length, `has_links`; each cell's text, halves,
  attributes and link; and whether `Window::row` and `Row::cell` agree.
- **Marks** (whether the mark moved, `full_refresh_since`, the rows
  `Row::changed_since` names) and **rows by identity** (`row_index`,
  including rows since dropped).
- **Everything the host is given,** in order: replies byte for byte, events,
  unhandled sequences, each call's result and error.
- **Probes,** on clones, for what no accessor shows: every query's reply
  (DECRQM, DSR, DA1/2/3, XTVERSION, DECRQSS, DECRQCRA, colour queries and
  more), the saved cursor and pen, character sets, tab stops, both pairs
  of margins (where origin mode's corners are), the pen and the open link
  (a glyph printed), what a selective erase leaves, the cluster the next
  character would join, REP, the kitty keyboard stacks.
- **The standalone types:** `Cells` under random edits, `CellRef::new`
  and `continues_cluster`.

Not compared: allocation failure itself (forcing one needs `unsafe`; the
sizes fux-vt refuses before allocating are compared instead); `Debug`
output, which a storage redesign changes on purpose (`RowId` and `Mark` are
compared by number); and what fux-vt tells no host: prompt marks, the raw
soft-wrap flag, link ids and picture numbers.

### The inputs

- **The corpus:** every recording in `fux-vt/compare/corpus/`, set up as a
  fux pane and with every option off, each step cut into pieces of 1 to
  2,048 bytes.
- **The ceiling,** with the corpus: the working tree's fux-vt alone, as a
  pane, each recording read 4 KiB at a time
  (`process_until_stop` until the read is taken): the call that does the
  most work, in fux-vt's work units (`fux_vt::work_spent`); and a resize of
  a full history at 50×200 to 80 columns, and back. Each is printed as a
  share of the most one call spends (the quota and the largest indivisible
  operation, `fux-vt/THREATS.md` 4.2), and past it the run fails.
- **Random cases** from fux-vt-compare's families
  (`fux-vt/compare/src/families.rs`, included by path, so a new family is
  drawn here too) at random sizes, histories and options, with resizes,
  `process_until_stop`, copies, and output cut anywhere.
- **Resize streams:** history full, a resize every few lines, wide glyphs,
  clusters, links and prompt marks, with reflow and without.
- **The limits:** OSC strings, links, clusters, parameters and sizes at and
  past their bounds; a size past capacity must be refused
  (`Error::Capacity`) with the screen unchanged.
- **The fuzz target** (`fuzz/fuzz_targets/oracle.rs` documents its input):
  output, resizes (refused ones too), frames, copies and grapheme runs at
  small sizes, or a case as `--replay` text. Start it from `--seeds`: the
  seeds carry the oracle's generators into the fuzzer.

No case may make a grid that holds more than 2 Mi cells unless fux-vt is
sure to refuse it (`case::safe`).

### A difference

Each part stops at its first difference, says how long it took to find,
and shrinks it for up to `--shrink` seconds: later steps cut, then steps,
then bytes, then sizes, history and options. It prints both sides of the
first field that differs (`pen.bold`, or a row, column and cell), the
shrunk case, and the file it wrote under `diff/target/oracle/` for
`--replay`. Standalone-type runs are not shrunk; their edits are listed.
