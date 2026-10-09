# fux-diff: fux beside its last release

An excluded workspace with its own lockfile. It holds two tools for changes
meant to leave behaviour as it was (a refactor, a speed-up, a rewrite):

- **`fux-diff`** builds the working tree's `fux`, `fux-vt` and `fuxix` and
  their last releases from crates.io (`baseline`, `baseline-vt`,
  `baseline-ix` in `Cargo.toml`) into one binary, feeds both the same
  random inputs and compares everything each gives back.
- **The oracle** (`oracle/`, `oracle.sh`, fuzz target in `fuzz/`) compares
  the working tree's fux-vt with fux-vt at a commit: see
  [The oracle](#the-oracle-fux-vt-beside-a-commit).

It forbids the same lints as fux, and its release profile checks arithmetic
for overflow. CI only checks that `fux-diff` passes fmt and clippy, and
allows that to fail (see [The baseline](#the-baseline)).

## fux-diff

```sh
cargo build --release --manifest-path diff/Cargo.toml
diff/target/release/fux-diff                                # every area
diff/target/release/fux-diff --scale 10 --seed 7 sessions layout
diff/target/release/fux-diff --list                         # what each area compares
diff/target/release/fux-diff --speed                        # fux-vt's parse time beside its last release
```

| Flag | Meaning |
| --- | --- |
| `AREA...` | the areas to run; all if none |
| `--seed N` | the seed (default 1). Each area derives its own from it, so one area run alone replays exactly |
| `--scale N` | multiplies each area's number of cases (default 1); with `--speed`, lengthens the streams |
| `--speed` | times fux-vt beside its last release instead of comparing |

Each area prints how much it found alike, or stops at its first difference
and prints the seed, both sides and what led to it (a session's last 40
events, a tree and its changes, a terminal's output). Exit status: 1 if an
area differed, 2 for a bad argument.

### Areas

| Area | Input | Compared |
| --- | --- | --- |
| `sessions` | whole sessions of random events: keys through every mode and overlay, pastes, commands with and without a client and with good and bad targets, program output, resizes, attaches, detaches, exits, paints, shutdowns | after every event: its result, every view, pane, workspace and tab, the outbox, the paste buffers and the bindings |
| `screens` | the same sessions | the above, and each client's composed screen painted from nothing and from its last screen, byte for byte |
| `commands` | random command lines | the command or usage error, usage message, label, and every word read as each kind of target |
| `layout` | random trees, weights 0 to `u32::MAX`, areas up to `u16::MAX` wide | placements, neighbours, resizes, splits, removals and swaps |
| `protocol` | random frames and byte streams, broken ones included | encoded bytes; frames, errors and bytes held, decoding whole and in pieces |
| `terminal` | fux-vt fed random output in pieces, with resizes | after every piece: replies, events, errors, every retained row, the cursor, modes, scroll region, changed rows. Leaves out what fux-vt 0.2.0 changed on purpose (SGR 5, 6, 8, 9, 25, 28, 29, 58, 59; CSI f, s, u; characters that join a cluster) |
| `input` | client input in pieces with Escape timeouts, key names, pastes, held command lines, pane input queues | decoded input and the Escape deadline, encoded keys and pastes, the queues |
| `copy` | screens and history from random lines; searches both ways; every copy-mode error | matches, copied text, row positions, each error's notice |
| `text` | random words, key names, config lines and files | `split`, `quote`, `join`, `shell_line`, key names, config results, JSON strings, base64 |
| `system` | errnos, nonblocking requests on pipes, sockets and PTYs, PTY sizes | fuxix's results, and the config file's path for every mix of `XDG_CONFIG_HOME` and `HOME` |

`--speed` parses five synthetic streams (ASCII, SGR, CJK, emoji, cursor
movement) and the recordings in `fux-vt/compare/corpus/` with both fux-vts
in turn, best of nine, by the thread's CPU time, so a busy machine slows
neither.

Not covered: the server's connection handling in `server.rs` (the walk,
`walk/`, and the integration tests cover it), and performance beyond
`--speed` (see `bench/`).

### The baseline

The baseline is the last release, so every deliberate change since then
differs too:

- **A change meant to alter behaviour** differs in the areas it touches;
  the others should still agree.
- **A change to fux's library API** can stop an area building for both
  copies. Each area's code is one macro (`stack!`) used for both: give the
  current side what it needs there and compare both in the same form. CI's
  "fux-diff builds" step may fail until then.
- **After each release,** bump the three `baseline` versions in
  `Cargo.toml` and `cargo update -p` them.

## The oracle: fux-vt beside a commit

`fux-vt-oracle` holds the working tree's fux-vt to what fux-vt did at a
commit, by default the merge base with `main`. Both get the same bytes,
resizes and API calls, and everything fux-vt's public API shows is compared
after every step. Any difference fails, shrunk to the smallest case that
shows it.

```sh
diff/oracle.sh                       # against the merge base with main
diff/oracle.sh 74e9769               # against another commit
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

### The commit, and the lockfile

The commit is `base-vt`, a git dependency at a `rev` in
`oracle/Cargo.toml`, so `Cargo.lock` names what was built and, once
fetched, it builds offline. `oracle.sh REF` pins `REF` there if it is
another commit and says so: commit the two changed files to keep the pin,
or `git checkout` them (or run `oracle.sh` with no `REF`) to go back. A
commit on a remote branch is fetched from GitHub; one only in this
repository by a `file://` URL, which works on this machine alone and must
not be committed.

### What is compared

Each side is read into one model (`oracle/src/model.rs`) through its own
public API (`oracle/src/side.rs`) after every step:

- **The screen's state:** size, cursor, pending wrap, every mode, the
  margins, the pen, the open link, history length, `storage_cells`, marks,
  `resize_report`, the options reported.
- **Every retained row:** identity and version, soft wrap, prompt mark,
  length, `text_len`, `has_links`; each cell's text, width, attributes,
  colours and link, through every getter.
- **Marks** (`changed_since`, `full_refresh_since`, dirty rows), **rows by
  identity** (`offset_for_row`, `row_by_id`, including rows since dropped),
  and **copies** (`Window::text`, random windows and selections).
- **Everything the host is given,** in order: replies byte for byte, events,
  unhandled sequences, each call's result and error.
- **Probes,** on clones, for what no accessor shows: every query's reply
  (DECRQM, DSR, DA1/2/3, XTVERSION, DECRQSS, DECRQCRA, colour queries and
  more), the saved cursor, character sets, tab stops, the cluster the next
  character would join, REP, the kitty keyboard stacks.
- **The standalone types:** `Cells` under random edits, `Cell::new`,
  `continues_cluster`, and the constants and limits.

Not compared: allocation failure itself (forcing one needs `unsafe`; the
sizes fux-vt refuses before allocating are compared instead);
`Error::IdentityExhausted` (2^64 rows); `Debug` output, which a storage
redesign changes on purpose (`RowId` and `Mark` are compared by number);
`process` and `process_with_replies`, which are `process_with` with less
kept.

**Adapters.** Where the two APIs part, give `side!` an argument for that
part, read both into the same model, and list it here. Today: the
modes, which the working tree reads with `Screen::mode` and the merge base
with a getter each; `Options::palette` and `Screen::colors_changed`, which the merge base lacks.
The working tree gets the option the case asks for; the base reports the
option as asked and no colour changed. A commit without an adapter for its
API does not build (`oracle.sh dfe1ffb`, before
`Options::setting_reports`, stops there).

### The inputs

- **The corpus:** every recording in `fux-vt/compare/corpus/`, set up as a
  fux pane and with every option off, each step cut into pieces of 1 to
  2,048 bytes.
- **Random cases** from fux-vt-compare's families
  (`fux-vt/compare/src/families.rs`, included by path, so a new family is
  drawn here too) at random sizes, histories and options, with resizes,
  `process_until_frame`, copies, and output cut anywhere.
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

### Exemptions

A feature the user approved adding to fux-vt since the commit changes what
its own sequences do, and nothing else. Each is a named exemption in
`oracle/src/exempt.rs`, an input filter: every byte given to both sides
goes through it, and it takes out exactly the feature's sequences (a C0
control inside one stays). Both sides get the same filtered bytes and
everything is compared as before. A sequence split between steps is held
until whole. `exempt`'s tests list what is taken out and what is kept.

| Feature | Sequences taken out |
| --- | --- |
| the palette, `Options::palette` | OSC 4, 5, 104, 105, 110 to 119; OSC 10 to 19 when they set a colour (a query alone stays) |
| reverse wraparound | `CSI ? 45 h/l`, `CSI ? 1045 h/l` |
| modes kept as xterm keeps them | DECSCLM (`?4`), DECSCNM (5), DECARM (8), DECNKM (66), DECBKM (67) |
| XTSAVE and XTRESTORE | `CSI ? Pm s`, `CSI ? Pm r` |
| LNM | `CSI 20 h/l` |
| DECID | `ESC Z` |
| DECALN | `ESC # 8` |
| left and right margins | DECLRMM (`CSI ? 69 h/l`) and its DECRQM; DECSLRM (`CSI Pl ; Pr s`) while DECLRMM is set, tracked as fux-vt would (otherwise it is SCOSC and passed on); DECIC and DECDC (`CSI Pn ' }`, `CSI Pn ' ~`) |
| protected glyphs and selective erase | DECSCA (`CSI Ps " q`), SPA and EPA (`ESC V`, `ESC W`), DECSED (`CSI ? Ps J`), DECSEL (`CSI ? Ps K`) |

An exempt mode is taken out of a DECSET, DECRST, SM or RM and the others
kept (`CSI ? 7;45 h` becomes `CSI ? 7 h`); its DECRQM goes whole. Only the
plain form fux-vt reads is touched; any other is passed on, as both sides
read it alike. On the corpus this takes out only zellij's `OSC 4` queries
and neovim's left and right margins.

**Memory diagnostics.** Since the compact cell (8-byte cells, long clusters
stored outside, history rows trimmed), `storage_cells` and a row's
`text_len` may be smaller than the commit's, never larger
(`observe.rs`, `larger`). `Cells`' `text_len` is still compared exactly.
Any other change to how cells are stored that moves these is an exemption
to approve and record, not to hide.

### A difference

Each part stops at its first difference, says how long it took to find,
and shrinks it for up to `--shrink` seconds: later steps cut, then steps,
then bytes, then sizes, history and options. It prints both sides of the
first field that differs (`pen.bold`, or a row, column and cell), the
shrunk case, and the file it wrote under `diff/target/oracle/` for
`--replay`. Standalone-type runs are not shrunk; their edits are listed.
