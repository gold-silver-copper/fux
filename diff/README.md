# fux-diff: fux beside its last release

An excluded workspace with its own lockfile. It builds two copies of fux into
one binary:

- the code in this repository (`fux`, `fux-vt` and `fuxix` by path);
- the last release of each, from crates.io (`baseline`, `baseline-vt` and
  `baseline-ix` in `Cargo.toml`).

It feeds both the same random inputs and compares everything each gives
back. It is for a change that means to leave behaviour as it was: a
refactor, a speed-up, a smaller or more idiomatic rewrite. It is not built or
run by fux's own gates.

Its only dependencies are fux, fux-vt and fuxix, twice each. It forbids the
same lints as fux.

The same workspace holds fux-vt's behaviour oracle (`oracle/`, with its fuzz
target in `fuzz/` and its command `oracle.sh`): the working tree's fux-vt
beside fux-vt at a commit rather than a release, compared on everything its
API shows after every step. See [the oracle](#the-oracle-fux-vt-beside-a-commit)
below.

## Commands

From the repository root:

```sh
cargo build --release --manifest-path diff/Cargo.toml

# Every area, at the default number of cases (about a minute of CPU):
diff/target/release/fux-diff

# Some areas, ten times as many cases, another seed:
diff/target/release/fux-diff --scale 10 --seed 7 sessions layout

# What each area compares:
diff/target/release/fux-diff --list
# fux-vt's parse time beside its last release, by thread CPU time:
diff/target/release/fux-diff --speed
```

- Each area prints what it found alike and how much it compared.
- A difference prints the area, the seed, what differed, both sides, and
  what led to it (a session's last 40 events, a tree and the changes made to
  it, a terminal's output, and so on).
- The exit status is 1 if any area differed.
- Each area draws from a seed of its own, derived from `--seed`, so running
  one area alone replays it exactly.
- A release build checks arithmetic for overflow, as a debug build does.

## Areas

| Area | What both are given | What is compared |
| --- | --- | --- |
| `sessions` | Whole sessions of random events: keys through every mode and overlay, pastes (too long, and past a pane's input limit), commands with and without a client and with targets that exist and do not, program output, resizes, attaches, detaches, exits, input read by the program, paints and shutdowns | After every event: what it returned, and every view (mode, notice, focus, `dirty`, maps), pane (name, title, size, queued input), workspace and tab (names and layout trees), the outbox, the paste buffers and the bindings |
| `screens` | The same sessions | Everything in `sessions`, and after every event the paint of each client's composed screen, from nothing and from its last screen, byte for byte |
| `commands` | Random command lines, half of them `select-*` lines | The command or usage error, the usage message, the label, and every word read as each kind of target |
| `layout` | Random trees, normalized and not, with weights from 0 to `u32::MAX`, in areas from empty to `u16::MAX` wide, on the screen and past its edge | Placements (panes and separators), neighbours in every direction, and resizes, splits, removals and swaps: what each returns and the tree it leaves |
| `protocol` | Random frames, up to twice the largest payload; random byte streams of frames, broken frames, stray headers and bad lengths | Encoded bytes (`encode`, `encode_into`, each stream's frames, `encode_input`); frames, errors and bytes held, decoding whole and in pieces; `check`; raw frames' paints, inputs and decodings |
| `terminal` | fux-vt fed random output (well-formed sequences, and some hostile or broken ones) in pieces, with resizes, with events and extended replies on and off. Only output both mean the same by: not what the current fux-vt changed on purpose (SGR 5, 6, 8, 9, 25, 28, 29, 58, 59; CSI f, s, u; characters that join a grapheme cluster), which fux-vt's own tests check against their models | After every piece: replies, events and errors, every retained row's identity, version, wrap flag, place and cells, the cursor, every mode, the scroll region, and the rows changed since the last look |
| `input` | Random client input in pieces, with Escape timeouts; random key names with modifiers; pastes; held command lines; pushes and reads on a pane's input queue | Decoded keys, pastes and focus changes, and the Escape deadline; each key's bytes in both cursor modes; each paste plain and bracketed; when a held line is due; the queue's results and contents |
| `copy` | Panes' screens and history from random lines; searches from random places, both ways; every copy-mode error in a session | Matches, the text each kind of selection copies, row positions; each error's outcome and notice (and that each is still reached) |
| `text` | Random lines of words, key names, config lines and files | `split`, `quote`, `join` and `shell_line` with and without fish; key names read and printed; each config line's result and the configuration left; config files; JSON strings and base64 |
| `system` | errno values, nonblocking requests on pipes, sockets and PTYs, PTY sizes, pipes | fuxix's errnos, results and reads that do not wait, window sizes, bytes through a pipe; the config file's path in every mix of `XDG_CONFIG_HOME` and `HOME`, each in a process of its own |

## When the two differ on purpose

- **A change meant to alter behaviour.** Expect a difference in the areas it
  touches. Run the others, which should still agree.
- **A change to fux's library API.** A renamed type or variant, for example,
  can stop the area that uses it from building for both copies. The code for
  both is one macro per area (`stack!`); give the current side what it needs
  there, and compare what both give back in the same form.
- **After each release,** bump the three `baseline` versions in `Cargo.toml`
  to it, and `cargo update -p` them.

## Not covered

- **The server's connection handling.** Reading and handling client frames,
  refusing connections, and painting on a timer all live in `server.rs`,
  behind a real socket and real processes. The walk (`walk/`) and the
  integration tests cover them.
- **Performance.** The areas compare only what fux does. `fux-diff --speed`
  times fux-vt beside its last release instead: five streams of output
  (ASCII, SGR, CJK, emoji, cursor movement), and real programs' output
  (`corpus`: the recordings in `fux-vt/compare/corpus/`), each parsed in
  turn by both, best of nine, by the CPU time of the thread, so a busy
  machine slows neither. `--scale` lengthens the streams.

## The oracle: fux-vt beside a commit

`oracle/` (`fux-vt-oracle`, a library and a binary), `fuzz/` (its fuzz
target) and `oracle.sh` (the command) hold fux-vt to what it did at a
commit: by default the merge base with `main`, so that work that redesigns
fux-vt's storage or adds fast paths can show it changed no behaviour. The
working tree's fux-vt and fux-vt at the commit get the same bytes, resizes
and API calls, and everything fux-vt's public API shows is compared after
every step. Any difference fails, shrunk to the smallest case that shows
it.

```sh
diff/oracle.sh                     # against the merge base with main
diff/oracle.sh 74e9769             # against any commit
diff/oracle.sh --cases 50000 --seed 7
diff/oracle.sh --replay diff/target/oracle/NAME.case
diff/oracle.sh --seeds diff/fuzz/corpus/oracle --cases 1000 --streams 5
diff/oracle.sh --help

# The fuzz target, as fux-vt-compare's harness runs it:
cargo +nightly fuzz run oracle --fuzz-dir diff/fuzz -O -a diff/fuzz/corpus/oracle -- \
  -dict=fux-vt/fuzz/terminal.dict -rss_limit_mb=2048 -timeout=25
```

`fux-vt/compare/run.sh quick` runs `oracle.sh` beside its other checks
(about 9 s of its 27 s here), and `run.sh fuzz` runs the fuzz target with
the others.

### The commit, and the lockfile

The commit is a git dependency of `oracle/Cargo.toml` (`base-vt`, fux-vt at
a `rev`), so `Cargo.lock` names the commit that was built, and once fetched
it builds with no network. `oracle.sh REF` pins `REF` there if it is
another commit, and says so: `oracle/Cargo.toml` and `Cargo.lock` change,
to commit if the pin is to stay, or to `git checkout` (or run `oracle.sh`
with no `REF`) to go back. A commit on a remote branch is fetched from
GitHub; one only in this repository, by a `file://` URL to it, which works
on this machine alone and is not to be committed.

### What is compared

Each side is read into one model (`oracle/src/model.rs`) through its own
public API (`oracle/src/side.rs`), after every step:

- **The screen's state:** size; cursor; pending wrap; every mode
  (`hide_cursor` to `insert_mode`, origin, autowrap, alternate screen,
  mouse mode and encoding, kitty flags, modifyOtherKeys, synchronized
  output, in-band resize, colour-scheme updates, focus reporting, cursor
  shape); the margins; the pen through `attributes` and through `bgcolor`
  and `inverse`; the open link; history length; `storage_cells`; the mark,
  as a number; `resize_report`; the options the parser reports; and that
  `row_from_bottom` ends where history and screen do.
- **Every retained row, history and screen:** identity and version (the
  exact numbers), soft wrap, prompt mark, length, `text_len`, `has_links`;
  each cell's text, halves, every attribute and colour (underline style and
  colour, blink, hidden, strikeout and the rest) through `attributes()` and
  through each of `CellRef`'s getters, and its link (URI, id, key); and
  that `Row::cell`, `Screen::cell`, `Screen::link`, `Screen::row_wrapped`
  and `Screen::starts_prompt` say what the row says.
- **Marks:** one taken as the parser was made, one after the step before,
  one every four steps: `changed_since`, `full_refresh_since`, and the rows
  `dirty_rows_since` and `dirty_live_rows_since` give.
- **Rows by identity:** `offset_for_row` and `row_by_id` for the bottom
  row, the screen's top, the cursor's row, the oldest, one between, and the
  oldest of the step before, which may be gone.
- **Copies:** `Window::text` of the whole screen and of the top of history;
  a `copy` step reads a random window whole (each `Window::cell`, `row`,
  `row_wrapped`) and copies a random selection with random limits.
- **Everything the host is given,** in order: replies byte for byte, events
  (titles, icon names, bells, clipboard, colour queries) and unhandled
  sequences (the title stack's `CSI 22/23 t` among them, which fux keeps);
  each call's result and error, `Display` text included.
- **Probes,** on clones of each parser, for what no accessor shows: every
  query's reply (DECRQM for every mode fux-vt knows and some it does not,
  DSR 5/6, DECXCPR, DA1/2/3, XTVERSION, kitty's flag query, the size
  queries, DECRQSS for the pen, cursor shape, margins and what it does not
  know, DECRQCRA of the screen, colour and colour-scheme queries); the saved
  cursor (DECRC) with the pen and origin mode it restores; the character
  sets (G0 and G1 drawn); the tab stops (a tab at a time across the row,
  and back); the cluster the next character would join; the character REP
  repeats; the kitty keyboard stack (popped one by one) and the alternate
  screen's.
- **The standalone types:** `Cells` under random edits of every kind
  (`set_text`, `set_cell`, `set`, `fill`, `resize`, `set_attributes`,
  collecting, equality) with its text budget; `Cell::new`;
  `continues_cluster`; and the constants and limits (`URI_LIMIT`,
  `ID_LIMIT`, `OSC_PAYLOAD_LIMIT`, `Cell::INLINE_CAPACITY` and
  `CLUSTER_CAPACITY`, `Identity::MAX_LEN`, `Cells::text_limit`,
  `UNICODE_VERSION`, the defaults, `UnderlineStyle`'s numbers, the errors'
  text).

Where the two sides' APIs part (a renamed method, a reshaped type), give
`side!` an argument for that part, read both into the same model, and list
the adapter here. **Adapters today:** `Options::palette` and
`Screen::colors_changed` (the palette, an approved feature; see
[Exemptions](#exemptions)), which the merge base has not: the working
tree is given the option a case asks for and its `colors_changed` is
compared; the merge base reports the option as asked, and no colour
changed. The palette's sequences are exempt, so a colour changed by
anything else differs. A commit with another API does not build until it has one: `oracle.sh
dfe1ffb`, from before `Options::setting_reports`, stops there.

### The inputs

- **The corpus:** every recording in `fux-vt/compare/corpus/` at its size,
  with its resizes, as fux sets up a pane (10,000 rows of history, every
  option fux sets) and with every option off; each step's output cut into
  pieces of 1 to 2,048 bytes, as reads of a PTY come, so pieces end inside
  sequences and characters.
- **Random cases:** output from fux-vt-compare's families
  (`fux-vt/compare/src/families.rs` and `rng.rs`, included by path, so a
  new family is drawn here too), a few families a case or all of them, at
  random sizes, histories and options, with resizes, `process_until_frame`
  steps, copies, and output cut anywhere.
- **Resize streams:** history filled past its limit, then a resize every
  few lines (one cell to 120 by 250), with wide glyphs, clusters, colours,
  links and prompt marks, now and then on the alternate screen or with the
  cursor up the screen; with reflow and without.
- **The limits:** OSC strings at and past 64 KiB; links at and past their
  URI and id bounds, and 2,200 or 70,000 of them, past the screen's count
  and bytes; clusters past their capacity, and rows of long clusters past
  the row's text; parameters and repeats past their bounds; and sizes past
  what a grid may hold, refused (`Error::Capacity`, before allocating) with
  the screen as it was.
- **The fuzz target:** coverage-guided bytes read as a case
  (`fuzz/fuzz_targets/oracle.rs` says how): output, resizes (refused ones
  too), frames, copies and runs of grapheme characters, at small sizes with
  a small history, so it fills; or a case as `--replay` text.
  `fux-vt-oracle --seeds DIR` writes random cases, resize streams and the
  limits there as seeds; `run.sh fuzz` writes 1,012 into the target's
  stored corpus (`fuzz/corpus/oracle`, ignored by git) before it runs.

No case may make a grid able to hold more than 2 Mi cells unless fux-vt is
sure to refuse the size (`case::safe`), so neither the generators nor the
shrinker make one that allocates gigabytes.

### Exemptions

A feature the user approved adding to fux-vt changes what the sequences it
reads do, and only those: the commit ignores them, or answers them
otherwise. Each such feature is a named exemption in `oracle/src/exempt.rs`,
an input filter: every byte both sides are given, each step's output and
each probe, goes through it first, and it takes out exactly the feature's
sequences (a C0 control inside one, which executes wherever it is, stays).
Both sides get the same filtered bytes, and everything is compared as
before; a sequence not named there is passed on whole. A sequence cut
between two steps is held until it is whole, and given with the step that
ends it. `exempt`'s tests list what is taken out and what is kept.

**Exemptions today** (phase 5 of the work to beat Ghostty's core, approved
by the user):

| Feature | Sequences taken out |
| --- | --- |
| the palette, `Options::palette` | OSC 4, 5, 104, 105 and 110 to 119; OSC 10 to 19 with a parameter other than `?` (a query alone stays: with no colour set it is an event, as before) |
| reverse wraparound | `CSI ? 45 h/l`, `CSI ? 1045 h/l` |
| modes kept as xterm keeps them | DECSCLM (`CSI ? 4`), DECSCNM (5), DECARM (8), DECNKM (66), DECBKM (67) |
| XTSAVE and XTRESTORE | `CSI ? Pm s`, `CSI ? Pm r` |
| LNM | `CSI 20 h/l` |
| DECID | `ESC Z` |
| DECALN | `ESC # 8` |
| left and right margins | `CSI ? 69 h/l` (DECLRMM) and its DECRQM; `CSI Pl ; Pr s` (DECSLRM) while DECLRMM is set, which the filter follows as fux-vt would: DECSET and DECRST of 69, DECSTR and RIS resetting it, XTRESTORE of it putting back what XTSAVE saved (while it is reset the sequence is SCOSC, and passed on); `CSI Pn ' }` and `CSI Pn ' ~` (DECIC and DECDC, the column edits the margins bound) |
| protected glyphs and selective erase | `CSI Ps " q` (DECSCA), `ESC V` and `ESC W` (SPA, EPA), `CSI ? Ps J` (DECSED) and `CSI ? Ps K` (DECSEL) |

A mode is taken out of a DECSET, DECRST, SM or RM and the modes beside it
kept (`CSI ? 7;45 h` is given as `CSI ? 7 h`); DECRQM of it is taken out
whole. Only the plain form fux-vt reads is touched (digits and `;`, a `?`
marker, then the sequence's intermediates, as fux-vt's parser takes them:
a parameter byte after an intermediate, or a C0 control, DEL or a byte
from 0x80 inside, read as fux-vt reads them); any other is passed on, as
both sides read it alike. On the corpus the filter takes out zellij's 512
`OSC 4 ; n ; ?` queries (8,888 bytes) and neovim's 15 `CSI ? 69 $ p`
(one a recording, 105 bytes), and nothing else.

**The memory diagnostics** (phase 3 of the same work, the compact cell,
approved with it): `storage_cells` and each row's `text_len` may be smaller
than the commit's, and never larger; everything else is compared exactly
(`observe.rs`, `larger`). The grid stores its cells in 8 bytes, a cluster
of 5 to 17 bytes outside the cell, and history rows trimmed to the cells
they use, so the counts of what it keeps change by design: a row keeps the
long clusters' text a row of 32-byte cells keeps, or less. `Cells`, which
hosts keep, is unchanged, and its `text_len` is compared exactly.

With the filter in, two bugs planted next to the exempt sequences were
each found at once (and never committed): DECRQM of DECAWM (`?7`, among
the probes beside the exempt `?4`, `?5`, `?8`) answering the opposite, in
the probes, 0.1 s; and ECH taking the pen's attributes, not its colours
alone, in the corpus (htop) and random cases, 0.0 s, shrunk to `CSI 4 m`,
`CSI X`. With every feature in, two more: OSC 2 taken by the palette (its
title event lost) with `Options::palette` on, in random cases, 0.1 s; and BS
going back two columns where reverse wraparound is off, in the corpus
(bash) and random cases, 0.1 s, shrunk to `rl`, BS.

### A difference

The run stops at the first difference in each part, says how long it took
to find, and shrinks it for up to `--shrink` seconds (60): the steps after
it cut, then steps, then bytes of each step's output taken out, then sizes,
history and options made smaller. It prints both sides of the first field
that differs (its path, as `the screen's state: mark` or `pen.bold`, or the
row, column and cell), the shrunk case, and where it wrote the case
(`diff/target/oracle/`) to replay with `--replay`. The standalone types'
runs are not shrunk: a difference there lists the run's edits.

Nine bugs were planted in fux-vt, one at a time, and never committed, and
`fux-vt-oracle` run with its defaults. Each was found, in well under a
second of running:

| Planted | Found by | Found in | Shrunk to |
| --- | --- | --- | --- |
| a coloured erase also sets dim | corpus, random | 0.6 s, 0.0 s | 1x1: `\e[105m\e[J` |
| reflow: a cursor reflowed past the last column is not left waiting to wrap | random, resizes | 0.0 s, 0.3 s | 2x1 with reflow: `f`, resize to 1x1 |
| reflow: one blank line too many dropped below the cursor | random, resizes | 0.0 s, 0.2 s | 2x1 with reflow: `\n`, resize to 1x1 |
| XTVERSION's reply loses its last byte | every part | 0.0 s to 0.1 s | 1x1 with an identity, no output (the probes ask) |
| a row's soft wrap changes and its version does not | random, resizes | 0.0 s, 0.4 s | 1x1, one row of history: `e`, then `l` |
| DECSC does not save the character sets | random | 0.1 s | 1x2: `\e(0`, then `\e[s` (the probes restore and draw) |
| a scroll does not ask readers for a full refresh | every part | 0.0 s to 0.1 s | 1x1: `\n` |
| a resize refused for capacity still moves the mark | limits | 0.0 s | 1x1, 2,000 rows of history: resize to 1x65535 |
| a shorter `Cells` keeps its old text | standalone | 0.0 s | (its edits, listed) |

The fuzz target, 60 s a bug (at a load of 30 to 150 on 12 cores):

- From an empty corpus, mutation alone found five: XTVERSION's byte in
  0.6 s, the soft wrap's version in 2.4 s, the full refresh in 7.8 s, and
  the two reflow off-by-ones in 32 s and 62 s. It missed the erase's
  attribute and the character sets, which need sequences random bytes
  rarely spell, and the refused resize, which its resizes could not then
  reach (so `fb` was added). It does not drive `Cells`.
- From the oracle's seeds (`--seeds`, as the harness starts it), every bug
  but `Cells` failed on a seed as the corpus loaded, before any mutation:
  the seeds carry the oracle's generators into the fuzzer, and mutation
  goes on from there.

### Not compared, and why

- **Allocation failure itself.** fux-vt maps a failed `try_reserve` to
  `Error::Capacity`, but making an allocation fail needs a global allocator
  that fails on demand, which is `unsafe` (`unsafe_code` is forbidden here).
  The oracle compares the sizes fux-vt refuses before allocating instead,
  each refusal's error and the whole screen after it.
- **`Error::IdentityExhausted`:** it needs 2^64 rows or versions.
- **`Debug` output** of `Parser`, `Screen`, `CellRef` and `Attributes`:
  their layouts, which a storage redesign changes on purpose, and nothing
  fux reads. `RowId` and `Mark` are compared by the number their `Debug`
  shows, their only view of it.
- **`Parser::process` and `process_with_replies`** are not called on their
  own: each is `process_with` with a sink that keeps less.

The memory diagnostics, `storage_cells` and `text_len`, *are* compared:
since the compact cell, as no larger than the commit's (see
[Exemptions](#exemptions)). Another change to how cells or long clusters
are stored that changes them is a change to approve and record as an
exemption, not to hide.
