# fux-vt-compare: fux-vt beside other terminal emulators

An excluded package with its own lockfile. It runs fux-vt beside a panel of
other terminal emulators ("engines"):

- engines linked into the binary: Ghostty's core (libghostty-vt), alacritty_terminal,
  libvterm, avt, wezterm-term and the vt100 crate;
- engines run as processes of their own: xterm.js (`@xterm/headless` under
  Node), tmux, and xterm itself under Xvfb.

Every engine gets the same output. After each piece of it, and each resize,
each engine's screen is compared with fux-vt's, cell by cell, along with the
cursor, the modes, soft wraps, the title, cursor reports and recent history.
fux-vt fails a case only where the engines outvote it (see "The vote"). A
failing case is shrunk to the smallest one that still fails and printed with
the command that replays it. `bench` times every engine on the same
workloads.

It is not built or run by fux's own gates. It needs Zig, a Ghostty
checkout, libvterm's source and, for the engines in their own processes,
Node, tmux, xterm and Xvfb. `run.sh` fetches the first three.

No engine is the truth. When the panel outvotes fux-vt, the difference says
where to look. xterm's behaviour and the standards decide which side is
right. Some differences are choices fux-vt makes on purpose, and each family
below records which.

## Running it

```sh
fux-vt/compare/run.sh                       # families expected to agree, 20000 cases, beside the panel
fux-vt/compare/run.sh engines               # which engines run here, which vote, what each cannot tell
fux-vt/compare/run.sh cases                 # the named cases beside every engine, with each engine's mark
fux-vt/compare/run.sh verdicts              # the families with a recorded verdict, beside xterm alone
fux-vt/compare/run.sh matrix                # family by engine: % of cases each engine differs from fux-vt
fux-vt/compare/run.sh bench                 # MB/s for every engine on every workload
fux-vt/compare/run.sh --list                # the families, their status and reasons
fux-vt/compare/run.sh survey                # each family alone: how often it fails, and the smallest failure
fux-vt/compare/run.sh run --family sgr --family text --cases 2000 --seed 7
fux-vt/compare/run.sh run --engines ghostty,libvterm,xterm   # any panel
fux-vt/compare/run.sh replay --engines all --size 1x5 'abcde\x08X'
fux-vt/compare/run.sh --no-reflow           # fux-vt set up as fux sets it up
fux-vt/compare/run.sh --cargo test          # any cargo subcommand, in the same environment
fux-vt/compare/run.sh --cargo clippy --all-targets -- -D warnings
```

The first run fetches Zig 0.16.0, Ghostty and libvterm into
`~/.cache/fux-vt-compare` (or `$FUX_VT_COMPARE_CACHE`). It also installs
`@xterm/headless` with `npm ci` in `node/` when Node is present. The first
build takes a few minutes (Ghostty by Zig, then wezterm-term). After that,
`fux-vt/compare/target/release/fux-vt-compare` can be run directly.

- `run` exits 1 if any case fails, and prints up to five distinct shrunk
  cases. Each one shows:
  - its families and the `replay` command;
  - the fields fux-vt is outvoted on;
  - each engine's verdict and differences;
  - fux-vt's screen beside the first engine that differs (`.` blank, `_`
    cursor, `↩` soft-wrapped, `≠` a row that differs).
- `cases` marks each engine: `+` agrees, `-` differs, `!` abstains.
- `--seed` replays a run exactly. Every random choice comes from it.
- `--engines` takes names joined by commas, or one of:
  - `panel`, the voters, which is the default for `run`, `survey`, `matrix`
    and `replay`;
  - `all`, the default for `cases` and `bench`;
  - `in-process`.

## The vote

Each engine says what it can tell (`Can` in `src/engine.rs`): `engines`
lists what each one cannot. Each engine is compared with fux-vt only on
those fields, field by field: each cell's text, width and every attribute,
the cursor, each mode, the title, the reports, each history row.

fux-vt is outvoted on a field when more of the engines that can tell it
share one other value than agree with fux-vt. A case fails at the first step
where fux-vt is outvoted on any field. So:

- An engine's quirk alone never fails a case unless it is the only engine
  on the panel.
- Engines that each differ from fux-vt in a way of their own don't outvote
  it.
- A tie doesn't outvote it.

An engine that fails or panics abstains for the rest of that case. The
report says why, and the run goes on: vt100 panics on wrapping a
one-row screen, and wezterm on HTS one past the edge. xterm.js abstains from
terminals narrower than 2 columns, which it can't be.

**The default panel** is every voter that runs here: ghostty, alacritty,
libvterm, avt and wezterm.

**The panel can be wrong.** xterm decides where it splits. In `autowrap`, it
outvotes fux-vt even though xterm agrees with fux-vt, so that family is
marked as differing for that reason (see `--list`). They are all in-process, and fast.
- **vt100 doesn't vote.** It is fux-vt's ancestor, and its inherited choices
  are what the vote is meant to catch.
- **xterm.js, tmux and xterm don't vote by default** because they are
  slower: about 100 cases a second instead of 2000. Add them with `--engines
  panel,xterm.js` and so on; `cases` uses every engine.

## Engines

The quirks each engine showed against fux-vt and the others are listed in
its own file (`src/engines/*.rs`), each with a `replay` command.

| Engine | What, and pins | Cannot tell | Notes |
| --- | --- | --- | --- |
| fux-vt | the subject, by path, set up as ratty sets it up (reflow, an identity, kitty keyboard, events) | — | `--no-reflow`: as fux sets it up |
| ghostty | libghostty-vt 0.2.1 over Ghostty `7aa95917`, built by Zig 0.16 (see "Setup") | — | mode 2027 on; history kept in bytes |
| alacritty | alacritty_terminal 0.26.0 | blink | synchronized updates applied at once (no event loop); a wide glyph on one column panics it, which the adapter repairs |
| libvterm | libvterm 0.3.3 from its release tarball, through a C shim (`src/engines/libvterm_shim.c`); modes and pending wrap read from the pinned source's `vterm_internal.h` | dim, underline colour, kitty | the shim guards five crashes, hangs and out-of-bounds reads that random cases reach (found with ASan and UBSan; each listed with a replay in its file) |
| avt | avt 0.18.0 | underline colour, hidden, keypad, bracketed paste, focus, kitty, title, reports | takes `&str`: the adapter carries split UTF-8 and turns invalid bytes into U+FFFD |
| wezterm | wezterm-term at `cab25161` (git) | pending wrap | replies come through a writer thread, synced with a paste marker |
| vt100 | vt100 0.16.2 | underline colour, blink, hidden, strikeout, autowrap, origin, focus, kitty, reports | doesn't vote |
| xterm.js | @xterm/headless 6.0.0 with addon-unicode-graphemes 0.4.0, `reflowCursorLine` on, one Node process for every terminal (`node/engine.mjs`) | underline colour, kitty | patches a crash in ED 1 (see its file) |
| tmux | the installed tmux (3.7c here): a private server, one session per terminal, read with `capture-pane -p -e -N -F` and `display -p` | kitty | a `sh` pane program copies bytes in raw mode; every step is synced with DA1 (`CSI c`), which no family sends |
| xterm | the installed xterm (XTerm 411 here) under one Xvfb per run, read by printing every page (`CSI ? 11 i`) through `printerCommand`, modes by DECRQM, resize by `CSI 8 t` | pending wrap, underline colour, kitty | the deciding vote for disputed families (`verdicts`); the style of a row's blank cells after its last drawn cell cannot be read, and xterm abstains from a case with SGR 58, which it lacks (see its file) |

## Setup and pins

| What | Pin | Why |
| --- | --- | --- |
| `libghostty-vt` | `=0.2.1` (`libghostty-vt-sys` 0.2.2 in `Cargo.lock`) | the Rust bindings |
| Ghostty | `7aa9591746ff` (2026-07-22), through `GHOSTTY_SOURCE_DIR` | the first commit built by Zig 0.16. The bindings pin `a887df42` (2026-07-11), which needs Zig 0.15. Between the two, the C API changed only in the kitty-graphics temporary-file option (built out here: `default-features = false`) and one new data key |
| Zig | 0.16.0 | Zig 0.15.2 cannot link anything on macOS 27 |
| macOS SDK | the newest one before 27, through an `xcrun` shim | Zig's bundled libc++ does not compile against the macOS 27.0 SDK. Zig asks `xcrun --sdk macosx --show-sdk-path`, which ignores `SDKROOT` |
| libvterm | 0.3.3, SHA-256 checked in `run.sh` | compiled by `build.rs` with the `cc` crate |
| alacritty_terminal, avt, vt100 | exact versions in `Cargo.toml` | |
| wezterm-term | git `rev` in `Cargo.toml` | not on crates.io |
| @xterm/headless | `node/package-lock.json` | |

To move to a newer Ghostty, update `libghostty-vt` and check its pinned
commit and Zig version. Then diff `include/ghostty/vt/` between that commit
and the one `run.sh` checks out, before changing `ghostty_commit`.

## What is compared

After creation and after every step, each engine is read into one
`Snapshot`. The rules for reading are in the `Engine` doc comment in
`src/engine.rs`:

- every visible cell: its text (the whole grapheme cluster), width (narrow,
  wide, wide tail) and style (foreground, background and underline colour;
  bold, dim, italic, underline of any style, blink of either speed, inverse,
  hidden, strikeout);
- each row's soft-wrap flag;
- the cursor, and whether a wrap is pending (a cursor waiting to wrap is in
  the last column with `pending_wrap` set);
- cursor visibility, DECAWM, DECOM, the alternate screen, DECCKM, DECKPAM,
  bracketed paste, focus reporting, and the kitty keyboard flags;
- the title;
- cursor position and status reports (`CSI r;c R`, `CSI 0 n`);
- history: the text and wrap flag of every row fux-vt keeps, against the
  engine's most recent rows.

What is normalized away, and why:

- **A printed space and an empty cell are both blank.** Engines store spaces
  differently.
- **The cell after a wide glyph has no text or style of its own.**
- **A spacer at the end of a row is a blank.** That is where a wide glyph
  that didn't fit would have started.
- **Underline style and blink speed count only as on or off.** fux-vt keeps
  no underline style, and some engines no blink speed.
- **Device attributes and mode reports are not compared.** They name the
  terminal.
- **An empty title on the engine's side is not compared.** Some engines
  clear the title on RIS. fux-vt reports titles as they are set and keeps
  none.
- **Engines keep far more history than fux-vt can fill in a case.** Only the
  rows fux-vt keeps are compared. Ghostty also needs this for another
  reason: without scrollback it leaves a stale soft-wrap flag on the row it
  recycles (`replay --engines ghostty --size 1x3 abcd`).
- **Cases that resize give fux-vt 10000 rows of history.** A screen that
  grows pulls rows back from history.
- **Random cases settle the cursor before each resize.** They send `CSI ? 7
  h`, CR, `.`, CR (`--newline-before-resize` in a replay). Engines choose
  differently, on purpose, on these points:
  - whether a pending wrap survives a reflow;
  - where the cursor goes after a line that exactly fills the new width;
  - whether blank rows come back from history;
  - whether a reflow happens with autowrap off.

  Named cases and replays don't settle the cursor.
- **Some generators avoid an engine's parsing quirk:**
  - no empty SGR parameter within a list, which Ghostty ignores where xterm
    reads it as 0;
  - no empty kitty `CSI =` mode, which wezterm drops.

## Families

`--list` prints them with their current status. A family **agrees** when no
seed tried finds a case where the panel outvotes fux-vt.

A family is **decided** (`Status::Decided`, a recorded verdict) when the
engines split and fux-vt follows a recorded choice: what the references say
(`references/README.md` says which settles what), or what xterm does where
xterm departs from them (listed in fux-vt's README, "Departures from the
references") or they are silent. A vote can't judge such a family: on these
points the default panel's majority is often the side fux-vt has chosen
against. So the default run leaves it out, and `verdicts` checks it beside
xterm alone: its named cases (each pins one point of the verdict), then
random cases from it with plain text, where any field xterm can tell must
equal xterm's. `cases` fails a named case in a decided family where xterm
differs. The reason says which reference and choice, where the engines
stand, and a `replay --engines all` that shows it.

Otherwise a family **differs**, with a recorded reason, which is one of:
- a fux-vt defect still to fix (most are from the audit of fux-vt 0.2.0,
  numbered F1–F10 in the reasons);
- a documented fux-vt choice;
- engines agreeing on something else for reasons of their own.

When a fix lands, run its family alone over several seeds:

```sh
run --family NAME --family text --cases 20000
```

Once it agrees, set its status to `Status::Agree` in `src/families.rs`, so
every later run keeps it agreeing.

**Random cases:**
- **Size:** 1–3 cells now and then, mostly up to 9×17, and sometimes up to
  40×100.
- **History:** 0, 3 or 50 rows, or 10000 when the case resizes.
- **Length:** up to 10 steps, of up to 10 snippets each.

**The shrinker** takes away steps, snippets, and whole characters of text
snippets. It never removes bytes from a sequence, because a sequence cut
short is a different sequence that belongs to no family.

## Speed

`bench` feeds each engine every workload in 4 KiB chunks, on a 50×200
screen with 10000 rows of history. It prints MB/s, the best of three runs.
The workloads are modelled on alacritty's vtebench: ascii, dense-cells,
medium-cells, cursor-motion, scrolling, scroll-region and unicode.

An engine linked in is timed on parsing and applying alone. An engine in its
own process also pays for the pipe to it, so its figure (marked `*`) is end
to end.

## Files

| File | What |
| --- | --- |
| `run.sh` | fetch and pin Zig, Ghostty and libvterm, install node deps, build, run |
| `build.rs` | compile libvterm and its shim |
| `src/main.rs` | commands |
| `src/engine.rs` | the `Engine` trait and its reading rules, `Can`, the engine list |
| `src/engines/*.rs` | one adapter per engine, each documenting its quirks; `pane.rs` is what tmux and xterm share (the pane program, syncing, SGR decoding) |
| `node/` | the xterm.js server and its pinned packages |
| `src/case.rs` | running, voting, generating and shrinking cases; reports |
| `src/families.rs` | the families: generators, statuses and reasons |
| `src/cases.rs` | the named cases |
| `src/snapshot.rs` | what is compared, field by field, and the side-by-side view |
| `src/bench.rs` | the workloads and the speed table |
| `src/escape.rs` | bytes as replayable text, and back |
| `src/rng.rs` | splitmix64, as in `diff/` |
