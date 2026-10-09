# fux-vt-compare: fux-vt beside other terminal emulators

An excluded package with its own lockfile. It feeds fux-vt and a panel of
other terminal emulators ("engines") the same output and compares their
screens after every piece of it and every resize: each cell, the cursor,
the modes, soft wraps, the title, cursor reports and recent history. fux-vt
fails only where the engines outvote it ("The vote"). A failing case is
shrunk to the smallest that still fails and printed with a command that
replays it.

Beside that, it replays real programs' output (`corpus/`), checks that
fux paints a pane exactly as the program drew it (`transparency`), runs
xterm's conformance suite (`esctest`), and measures speed, instructions and
memory against the other engines.

No engine is the truth. When the panel outvotes fux-vt, the difference says
where to look. xterm and the standards decide who is right, and fux-vt's
deliberate choices are recorded with each family.

fux's own gates and CI don't build or run it. It needs Zig, a Ghostty
checkout and libvterm's source, which `run.sh` fetches, and, for the
engines in their own processes, Node, tmux, xterm and Xvfb.

## Running it

`run.sh` sets up the environment, builds, and runs `fux-vt-compare` with
its arguments, or one of the harness commands in `harness.sh`:

| Command | When | Budget | Runs |
| --- | --- | --- | --- |
| `run.sh quick` | before a commit | 60 s | `corpus` beside xterm and the panel; `corpus --subject ghostty`; `transparency`; `run --cases 2000`; `cases`; `encoders`; the oracle (`diff/oracle.sh`, see [`diff/README.md`](../../diff/README.md)) |
| `run.sh full` | before a PR | 600 s | `quick`; `run --cases 20000` with and without `--no-reflow`; `esctest`; `esctest --terminal ghostty --beside fux-vt`; `fux-bench --against main`; `footprint`; `bench --instructions` |
| `run.sh deep` | before a release, or when hunting | none (prints an estimate) | `full`; `verdicts` for seeds 1–20 (`FUX_DEEP_SEEDS`); `esctest --in-fux`; `transparency --multiplexers`; `fux-bench feel` and `info`; 10 minutes of fuzzing |
| `run.sh fuzz [MINUTES]` | by hand | MINUTES (10) | every fuzz target in `harness.sh`, sharing the time; a crash is minimized (`cargo fuzz tmin`) and listed, to be made a test |
| `run.sh scoreboard` | after any of them | seconds | the last results, gathered (see "The scoreboard") |

Each ends with a summary of its checks and exits 1 if any failed, or says
so if it ran over its budget. Independent checks run side by side, while
whatever times itself or counts instructions runs alone. Results, logs
(`logs/NAME.log`), a stamp for each check (commit, time, seconds, exit
status) and the fuzz ledger (`fuzz.jsonl`) go to
`fux-vt/compare/target/harness`, or `$FUX_HARNESS_OUT`.

The single commands:

```sh
fux-vt/compare/run.sh                       # run: 20000 random cases from the families expected to agree
fux-vt/compare/run.sh run --family sgr --family text --cases 2000 --seed 7
fux-vt/compare/run.sh run --engines ghostty,libvterm,xterm   # any panel
fux-vt/compare/run.sh --no-reflow           # fux-vt without reflow, its default
fux-vt/compare/run.sh --list                # the families, their status and reasons
fux-vt/compare/run.sh engines               # which engines run here, which vote, what each cannot tell
fux-vt/compare/run.sh cases                 # the named cases beside every engine
fux-vt/compare/run.sh verdicts              # the decided families, beside the engines that decide them
fux-vt/compare/run.sh survey                # each family alone: how often it fails, the smallest failure
fux-vt/compare/run.sh matrix                # family by engine: % of cases each differs from fux-vt
fux-vt/compare/run.sh replay --engines all --size 1x5 'abcde\x08X'
fux-vt/compare/run.sh corpus                # the recordings beside xterm and the panel
fux-vt/compare/run.sh corpus --show vim     # one recording, with fux-vt's screen at its end
fux-vt/compare/run.sh inventory > fux-vt/compare/corpus/INVENTORY.md
fux-vt/compare/run.sh transparency          # every recording directly and through fux
fux-vt/compare/run.sh esctest               # xterm's conformance suite
fux-vt/compare/run.sh encoders              # key, mouse, focus and paste encoders beside libghostty-vt's
fux-vt/compare/run.sh bench                 # MB/s for every engine on every workload
fux-vt/compare/run.sh bench --instructions  # instructions per byte, which load does not move
fux-vt/compare/run.sh footprint             # memory per cell and per row of history
fux-vt/compare/corpus/record.sh [NAME...]   # record the corpus again (see "The corpus")
fux-vt/compare/run.sh --cargo test          # any cargo subcommand, in the same environment
fux-vt/compare/run.sh --cargo clippy --all-targets -- -D warnings
```

`fux-vt-compare --help` (and `esctest --help`) gives every option. After
the first build, `fux-vt/compare/target/release/fux-vt-compare` runs
directly.

- `--engines LIST`: engine names joined by commas, or:
  - `panel`: the voters that run here, the default for `run`, `survey`,
    `matrix` and `replay`;
  - `all`: every engine that runs here, the default for `cases` and `bench`;
  - `in-process`: the default for `footprint` and `bench --instructions`.

  `corpus` defaults to `xterm,panel`, and `verdicts` to each family's
  deciding engines.
- `--seed N` replays a run exactly; every random choice comes from it.
- `replay` takes each step as output, written as `run` prints it
  (`'\e[1mX'`), or `resize:RxC`. It also takes `--size RxC` (default 6x20),
  `--history N` and `--newline-before-resize`.

### Reading a failure

`run` exits 1 if any case fails, and prints up to five distinct shrunk
cases. Each shows:

- its families and the `replay` command that reproduces it;
- the fields fux-vt is outvoted on;
- each engine's verdict and differences;
- fux-vt's screen beside the first engine that differs: `.` blank, `_` the
  cursor, `↩` a soft-wrapped row, `≠` a row that differs.

`cases` and `corpus` mark each engine: `+` agrees, `-` differs, `!`
abstains. Run the `replay` with `--engines all` to see every engine,
xterm included. Each engine's file in `src/engines/` lists its known
quirks, each with a `replay`.

## The vote

Each engine says which fields it can tell (`Can` in `src/engine.rs`; `run.sh
engines` lists what each cannot). Only those fields are compared, one by
one: each cell's text, width and every attribute, the cursor, each mode,
the title, the reports and each history row.

fux-vt is outvoted on a field when more of the engines that can tell it
share one other value than agree with fux-vt (`case::outvoted_on`). A case
fails at the first step where fux-vt is outvoted on any field. So:

- one engine's quirk never fails a case, unless it is the only engine on
  the panel;
- engines that each differ from fux-vt in their own way don't outvote it;
- a tie doesn't outvote it.

**A blank's style is a choice.** When an engine erases, inserts, deletes or
scrolls in a blank, it decides which parts of the pen the blank keeps:

- xterm and libvterm keep its colours alone, as fux-vt does (DEC STD 070
  gives an erased cell the empty rendition; xterm's `ClearCells` keeps the
  colours, `bce`);
- Ghostty, alacritty, xterm.js and tmux keep its background alone;
- avt, wezterm and vt100 keep its attributes too.

So on a cell that is blank in fux-vt and in the engine, an engine votes
only on the parts it makes as xterm does (`Blanks` in `src/engine.rs`;
`engines` lists the rest). Its differences there are still shown.

An engine that fails or panics abstains for the rest of the case. The
report says why, and the run goes on.

**The default panel** is every voter that runs here: ghostty, alacritty,
libvterm, avt and wezterm, all in this process and fast.

- **vt100 doesn't vote.** It is fux-vt's ancestor, and its inherited
  choices are what the vote is meant to catch.
- **xterm.js, tmux and xterm don't vote by default.** They run in
  processes of their own, about 20 times slower. Add them with `--engines
  panel,xterm.js` and so on. `cases` uses every engine.
- **The panel can be wrong.** Where it splits, xterm and the references
  decide: such a family is "decided" and checked by `verdicts` (see
  "Families").

### Another subject

`--subject NAME` (for `run`, `cases`, `corpus` and `replay`) judges another
engine in this process, such as ghostty, in fux-vt's place, exactly as
fux-vt is judged. fux-vt then joins the panel: where the list names
`fux-vt`, else in the subject's place (`panel` and `all` hold Ghostty). A
field the subject cannot tell is compared with no one. The statuses of the
families, named cases and recordings are fux-vt's, so they are not applied
to another subject. With one, `run`, `cases` and `corpus` report its
outcomes and fail only on an error.

`corpus --subject` and `cases --subject` judge fux-vt too, from the same
screens (`case::rejudge`), and list where each one is outvoted and the
other is not, each with the command that shows it. `--json FILE` writes it
all, including both scores, in recordings and in points (each judged
comparison):

```sh
fux-vt-compare corpus --subject ghostty --json corpus-ghostty.json
fux-vt-compare cases --subject ghostty
fux-vt-compare replay --subject ghostty --engines fux-vt --size 1x4 '\e[?2026h' '\e[!p'
```

## Engines

Each engine's adapter is `src/engines/NAME.rs`. Its doc comment lists the
engine's quirks against fux-vt and the others, each with a `replay`.
`run.sh engines` prints what each one cannot tell and how its blanks
differ from xterm's.

| Engine | What, and pin | Votes | Notes |
| --- | --- | --- | --- |
| fux-vt | the subject, by path, set up as `fux::pane::OPTIONS` with reflow and an identity, as fux and ratty set it up | — | `--no-reflow`: without reflow, fux-vt's default, leaving out the families that need it |
| ghostty | libghostty-vt `=0.2.1` over Ghostty `7aa95917`, built by Zig 0.16 | yes | mode 2027 on; history kept in bytes |
| alacritty | alacritty_terminal `=0.26.0` | yes | synchronized updates applied at once (no event loop) |
| libvterm | libvterm 0.3.3 through a C shim (`src/engines/libvterm_shim.c`) | yes | the shim guards crashes, hangs and out-of-bounds reads that random cases reach |
| avt | avt `=0.18.0` | yes | takes `&str`: the adapter carries split UTF-8 and turns invalid bytes into U+FFFD |
| wezterm | wezterm-term at git `cab25161` | yes | replies come through a writer thread, synced by a marker |
| vt100 | vt100 `=0.16.2` | no | fux-vt's ancestor; a speed baseline |
| xterm.js | `@xterm/headless` 6.0.0 with addon-unicode-graphemes 0.4.0, one Node process (`node/engine.mjs`) | on request | links read from its core, not the public API |
| tmux | the installed tmux: a private server, one session per terminal, read with `capture-pane` | on request | every step synced by DA1, which no family sends |
| xterm | the installed xterm under one Xvfb per run, read by printing every page through `printerCommand` | on request | decides most disputed families (`verdicts`) and the corpus; abstains from a case with SGR 58, which it lacks |

tmux and xterm share the pane program, syncing and SGR decoding in
`src/engines/pane.rs`.

### Adding an engine

1. Write `src/engines/NAME.rs`: an `Engine` (`process`, `resize`,
   `snapshot`, following the reading rules in `Engine`'s doc comment in
   `src/engine.rs`), and a `KIND` that gives its `Can`, its `Blanks`,
   whether it votes (`panel`), whether it runs `in_process`, `available`
   and `make`.
2. Add it to `src/engines/mod.rs` and to `ENGINES` in `src/engine.rs`.
3. Run `cases --engines NAME` and `run --engines panel,NAME`, and record
   each quirk the panel outvotes it on in its doc comment, with a `replay`.

## Setup and pins

The first `run.sh` fetches Zig, Ghostty and libvterm into
`~/.cache/fux-vt-compare` (or `$FUX_VT_COMPARE_CACHE`), and installs
`@xterm/headless` with `npm ci` in `node/` when npm is present. The first
build takes a few minutes.

| What | Pin | Why |
| --- | --- | --- |
| `libghostty-vt` | `=0.2.1` (`libghostty-vt-sys` 0.2.2 in `Cargo.lock`), `default-features = false` | the Rust bindings |
| Ghostty | `7aa9591746ff`, `ghostty_commit` in `run.sh`, through `GHOSTTY_SOURCE_DIR` | the first commit Zig 0.16 builds. The bindings pin `a887df42`, which needs Zig 0.15. Between the two, the C API changed only in the kitty-graphics temporary-file option (built out here) and one new data key |
| Zig | 0.16.0, SHA-256 checked in `run.sh` | Zig 0.15 cannot link on macOS 27 |
| macOS SDK | the newest one before 27, through an `xcrun` shim | Zig's bundled libc++ does not compile against the macOS 27 SDK, and Zig asks `xcrun --sdk macosx --show-sdk-path`, which ignores `SDKROOT` |
| libvterm | 0.3.3 release tarball, SHA-256 checked in `run.sh` | compiled by `build.rs` with the `cc` crate; modes and pending wrap read from its `vterm_internal.h` |
| alacritty_terminal, avt, vt100 | exact versions in `Cargo.toml` | |
| wezterm-term | git `rev` in `Cargo.toml` | not on crates.io |
| @xterm/headless | `node/package-lock.json` | |

To move to a newer Ghostty, update `libghostty-vt` and check its pinned
commit and Zig version. Then diff `include/ghostty/vt/` between that commit
and the one `run.sh` checks out before changing `ghostty_commit`.

## What is compared

After creation and after every step, each engine is read into one
`Snapshot`, by the rules in the `Engine` doc comment (`src/engine.rs`):

- every visible cell: its text (the whole grapheme cluster), its width
  (narrow, wide, wide tail), its style (foreground, background and
  underline colour; bold, dim, italic, underline, blink, inverse, hidden,
  strikeout) and its OSC 8 link: the URI, and which cells share one link.
  Engines name links in their own ways, so the names are not compared;
- each row's soft-wrap flag, and whether a prompt starts on it (OSC 133 A);
- the cursor, and whether a wrap is pending;
- cursor visibility, DECAWM, DECOM, the alternate screen, DECCKM, DECKPAM,
  bracketed paste, focus reporting, synchronized output (2026), in-band
  resize (2048) and the kitty keyboard flags;
- the title;
- cursor position and status reports (`CSI r;c R`, `CSI 0 n`);
- history: the text and wrap flag of every row fux-vt keeps, against the
  engine's most recent rows.

Normalized away:

- **A printed space and an empty cell are both blank.** Engines store them
  differently.
- **A wide glyph's second cell has no text, style or link of its own.**
- **A spacer at the end of a row is a blank.** It is where a wide glyph that
  didn't fit would have started.
- **Underline style and blink speed count only as on or off.** The
  engines' underline styles are not read yet.
- **Device attributes and mode reports are not compared.** They name the
  terminal.
- **An empty title on the engine's side is not compared.** Some engines
  clear the title on RIS.
- **Only the history rows fux-vt keeps are compared.** Engines keep far
  more than a case can fill. Cases that resize give fux-vt 10000 rows,
  since a screen that grows pulls rows back from history.
- **Random cases settle the cursor before each resize.** They send `CSI ? 7
  h`, CR, `.`, CR (`--newline-before-resize` in a replay), because engines
  choose differently whether a pending wrap survives a reflow, where the
  cursor goes after a line that exactly fills the new width, whether blank
  rows come back from history, and whether autowrap off still reflows
  (`Case::newline_before_resize`). Named cases and replays don't settle.
- **Some generators avoid one engine's parsing quirk**, each noted in
  `src/families.rs`: for example, no empty SGR parameter within a list,
  which Ghostty ignores and xterm reads as 0.

## Families

The random cases come from families of sequences (`src/families.rs`).
`--list` prints each with its status:

- **Agree** (`Status::Agree`): no seed tried finds a case where the panel
  outvotes fux-vt. `run` checks these by default.
- **Decided** (`Status::Decided`): the engines split, and fux-vt follows a
  recorded choice. That choice is what the references say
  ([`references/README.md`](../../references/README.md) says which settles
  what), or what xterm does where it departs from them (fux-vt's
  [README](../README.md), "Departures from the references") or they are
  silent. Its reason (`why`) names the reference and where the engines
  stand, and `by` names the engines that decide it: xterm for the VT
  families, and for a feature xterm lacks (OSC 133), the engines that follow
  the feature's spec. The default panel's majority is often the side
  fux-vt chose against, so `run` leaves these out. `verdicts` runs each
  family's named cases, then random cases from it, beside its deciding
  engines. `cases` fails a named case of a decided family where those
  engines outvote fux-vt.
- **Differs** (`Status::Differs`): the panel splits on a documented fux-vt
  choice, or on something the engines agree on for reasons of their own,
  or fux-vt has a defect still to fix. The reason says which.

When a fix lands, run its family alone over several seeds:

```sh
fux-vt/compare/run.sh run --family NAME --family text --cases 20000 --seed N
```

Once it agrees, set its status to `Status::Agree`, so every later run
keeps it agreeing.

Random cases are mostly up to 9×17, sometimes 1–3 cells and sometimes up
to 40×100. They have 0, 3 or 50 rows of history (10000 when they resize)
and up to 10 steps of up to 10 snippets. The shrinker takes away steps,
snippets and characters of text, never bytes from inside a sequence: a
sequence cut short belongs to no family.

`src/families.rs` is also included by path by `diff/oracle` for its random
cases, so it may use nothing but `crate::rng`, and must pass `diff`'s
lints.

## The corpus

`corpus/` holds what real programs wrote to a terminal while keys were
typed into them, byte for byte. Each scenario has:

- `NAME.bin`: the bytes;
- `NAME.json`: the program, its version, the command, the size, the
  environment, the replies fux-vt gave, and each step's keys and where its
  output ends;
- `corpus/keys/NAME.keys`: the keys, headed by a comment saying what the
  scenario does.

### Recording

`record` runs a program on a PTY (40x120, or `--size`) with
`TERM=xterm-256color`, as fux runs a pane:

- A fux-vt parser set up as a fux pane's reads the output, and its replies
  are written back as fux writes them. So a program that asks (DA1,
  DECRQM, a cursor report) gets fux's answer, and one that asks what fux
  doesn't answer gets nothing. Colour queries get a fixed white-on-black,
  dark answer, never the recording user's colours.
- Keys are typed as fux gives them to a pane: decoded as from a legacy
  terminal and encoded in the key mode the program asked for (kitty
  keyboard, modifyOtherKeys, cursor keys).
- Step 0 is the program starting. Each line of keys is then a step, and
  the step ends when the program has been quiet for 400 ms.
- In a keys file:
  - keys are written as `replay` writes output (`\e`, `\r`, `\x03`,
    `\u{…}`);
  - `#` starts a comment;
  - `!quiet MS` changes how long a step waits for quiet, and `!start MS`
    how long the program has to start (1500 ms by default);
  - `!wait` only waits;
  - `!resize RxC` resizes, as fux resizes a pane.
- After the last step the program has two seconds to exit, then gets
  SIGHUP, then SIGKILL.

`corpus/record.sh [NAME...]` records every scenario again, or those named.
It is run by hand only. Everything a program can show comes from
`/tmp/fux-corpus`:

- a HOME with a minimal configuration for each program;
- copies of files from this repository and generated text, including the
  man pages `corpus/fux-corpus.1`, `fux-corpus-tables.1` and
  `fux-corpus-long.7`;
- a generated git repository with a made-up author and fixed dates;
- cargo projects and a tree of files.

The environment is only what the manifest lists, plus `TERM`, and times are
in UTC. What the setup cannot keep out is scrubbed (`--scrub`): host names
in `file://` URIs, and file owners in ranger. Every recording is checked
for the login and host names, `$HOME`, git's user name and email, and the
remote's account. A recording that holds any of them is moved to
`/tmp/fux-corpus/dropped`, and the run fails. At the end the inventory is
made again and its changed rows printed for review.

To add a recording:

1. Write `corpus/keys/NAME.keys`.
2. Add a case for NAME to `scenario` in `corpus/record.sh`, and add NAME
   to its `all` list.
3. Run `corpus/record.sh NAME`.
4. Give NAME a status in `STATUSES` (`src/corpus.rs`). A recording without
   one fails `corpus`.
5. Regenerate `corpus/INVENTORY.md`.

### Replaying

`corpus` replays each recording through fux-vt and the engines (default:
xterm and the panel), as a case of the recording's size, with one step for
each step recorded. It compares them after every step, as `run` does. fux-vt
is set up as fux sets up a pane (reflow, kitty keyboard, no identity),
with fux's 10000 rows of history.

- **xterm decides what it can tell.** A field xterm tells that differs
  from fux-vt fails the step.
- **The panel decides the rest**: fields xterm cannot tell (underline
  colour, pending wrap, the kitty flags), and every field once xterm
  abstains (from SGR 58 on). A field the panel outvotes fux-vt on while
  xterm agrees with fux-vt is shown in the marks (`-alacritty`), not
  failed.
- **A resize is judged by the program's answer to it.** A resized step is
  two steps of the case: the resize, then the program's output. Only the
  second is judged, because right after a resize each engine shows its
  own way of resizing. fux's panes reflow, as Ghostty, wezterm and libvterm
  do; xterm doesn't.

Each recording has a status, as a family has (`STATUSES` in
`src/corpus.rs`): expected to agree, or differing for a recorded reason
that says at which step and why, with a command that shows it. `corpus`
exits 1 if a recording expected to agree fails, or one has no status.
`--json FILE` writes the results. The counts are in
[`scoreboard/SCOREBOARD.md`](scoreboard/SCOREBOARD.md).

### The inventory

`inventory` lists every sequence the recordings send, normalized: numbers
that only place the cursor or pick a colour are `n`, and each mode and SGR
attribute gets a row of its own. Each row has how often it was sent, by
which programs, and what fux-vt does with it. What fux-vt reports
(`Sink::unhandled`) or answers is observed. What it consumes without a word
is named from its source in `src/inventory.rs`, which must follow fux-vt.
`corpus/INVENTORY.md` is the output.

## Transparency

A program inside fux should look exactly as it does with no fux in between.
`transparency` checks that for every recording:

```sh
fux-vt/compare/run.sh transparency                    # every recording, read by Ghostty; exit 1 on a difference
fux-vt/compare/run.sh transparency --engines in-process vim   # read by each engine in this process
fux-vt/compare/run.sh transparency --chunk 64         # compared every 64 bytes too
fux-vt/compare/run.sh transparency --size 3x10 '\e]133;A\x07$ '   # given output, as replay takes it
fux-vt/compare/run.sh transparency --multiplexers     # tmux and zellij scored beside fux
fux-vt/compare/run.sh transparency --json FILE
```

The output goes to two terminals of one kind (Ghostty, unless `--engines`
names others in this process):

1. **Directly**, at the recording's size.
2. **Through fux**, used as a library as its server runs it (`Through` in
   `src/transparency.rs`). A `Session` with one pane and one client a row
   taller, so the pane above the bar has the recording's size. Output is
   read into the pane as the server reads it, and the client's terminal
   gets what `fux attach` writes first (`client::ENTER`), the server's
   queries, and every paint, made as `Server::paint` makes it.

Then the pane's rectangle of the client's screen is compared with the
direct screen, field by field, on what the engine can tell and a screen
shows (`SHOWN`): cells, prompt rows, cursor visibility, and the cursor
where it shows. Soft wraps, pending wrap, modes, the title, reports and
history are left out, because fux keeps them for the pane.

- **Colours are compared as they show.** fux paints a program's palette
  and default colours (OSC 4, 10, 11) as RGB and never sets them on the
  client's terminal. Where the engine can tell its colours (Ghostty), the
  direct side's colours are read as they now show. The run fails if the
  client's terminal's own colours ever change.
- **Where it compares:** after each step, and before each BSU and after
  each ESU of synchronized output; `--chunk N` adds a point every N bytes.
  While the pane holds a frame, fux shows the screen from before it by
  design, so that point is not compared.
- **What the mirror leaves out:** paints are made at every point, not
  16 ms apart; a client never stops reading; the client's answers to the
  server's queries are not sent back.
- **A difference is fux's, or fux-vt and the engine reading the program
  apart.** Both sides are read by one engine. Where fux-vt reads the
  program otherwise than the engine, fux paints what fux-vt has, and that
  difference is recorded with its reason in `KNOWN` (`src/transparency.rs`).
  Any other difference fails.

`run.sh --cargo test` checks every recording beside Ghostty
(`every_recording_is_transparent`).

### Other multiplexers

`--multiplexers` replays each recording that does not resize through tmux
and zellij, if installed, and scores them beside fux. The score is the
share of recordings and of steps whose pane matches the direct screen at
the end of each step. Their differences are scored, not failed. Each runs
as its own server with nothing of the user's:

- **tmux**: `-S` socket in the harness's directory, `-f /dev/null`, its
  status line below the pane. A step is read once a renamed session's
  mark shows in the status line, then 20 ms of quiet.
- **zellij**: its own configuration, a one-pane layout with no bars, its
  own `HOME`, data directory and `ZELLIJ_SOCKET_DIR`. A step is read after
  200 ms of quiet.

The client runs on a PTY with `TERM=xterm-256color` and
`COLORTERM=truecolor`. What it writes goes to the reference engine, whose
replies go back up the PTY. The pane runs the pane program in
`src/engines/pane.rs`.

`--json` writes `{"check": "transparency", ...}` for each engine and
recording, or `{"check": "transparency-multiplexers", ...}` for each
multiplexer; the fields are in `src/transparency.rs`. A multiplexer that
is not installed is `skipped`, and one that fails on a recording reports
`error` and gets no score. The scores are on the scoreboard.

## esctest

`esctest` runs esctest2 (`references/xterm/esctest2`, cloned by
`references/fetch.sh`), xterm's conformance suite by George Nachman and
Thomas E. Dickey. Each test writes to its terminal and reads reports back:
the cells by DECRQCRA checksums, the cursor by DSR, modes by DECRQM, and
settings by DECRQSS.

```sh
fux-vt/compare/run.sh esctest                    # every test against fux-vt; exit 1 on a mismatch with the list
fux-vt/compare/run.sh esctest DECSTBM            # tests whose name contains it (a Python regex)
fux-vt/compare/run.sh esctest --show --replays CUP   # each failure's message, and its bytes as a replay
fux-vt/compare/run.sh esctest --in-fux --xterm   # also in a real fux pane, and in a real xterm
fux-vt/compare/run.sh esctest --json FILE --logs DIR
fux-vt/compare/run.sh esctest --terminal ghostty # Ghostty's core under test
fux-vt/compare/run.sh esctest --terminal ghostty --beside fux-vt   # with fux-vt beside it, test by test
```

- **Directly** (`src/esctest.rs`): each area (a test class) is one esctest
  process on a 25×80 PTY. Its terminal is a fux-vt parser set up as fux's
  panes are, plus rectangle checksums and DECXCPR (`ExtendedReplies`),
  which panes leave off. Areas start on a fresh terminal and run in
  parallel, up to `--jobs` (one per CPU by default). A read waits
  `--timeout` (1 s) for its reply, so a missing report fails only that
  test. An area past `--limit` (120 s) is stopped, and its unfinished tests
  fail.
- **Options passed to esctest:** `--expected-terminal=xterm` (fux-vt
  follows xterm), `--max-vt-level=4` (a VT420, xterm's default and the
  level with DECRQCRA), and no `--options` (UTF-8, no window operations, like
  a default xterm). VT520 tests are skipped.
- **The list**, `esctest-expected.txt`: every failing test with its reason,
  one of `departure`, `spec`, `not-implemented`, `xterm-too` or `bug` (the
  file's header defines them). The run fails on an unlisted failure and on
  a listed test that passes, so the list never goes stale. A line tagged
  `[fux]` fails only in a pane, and `[direct]` only directly.
- **In fux** (`--in-fux`): builds fux and runs each area in the only pane
  of a fux server of its own (under `/tmp/fux-esctest-*`, never the
  user's), with a client on a 26×80 PTY. Panes don't answer DECRQCRA, so
  tests that read cells are skipped. A test that passes in one place and
  fails in the other is printed, and points at fux.
- **In xterm** (`--xterm`): each area also runs in a real xterm under Xvfb,
  set up as esctest's README says, with DECRQCRA allowed and empty cells
  counted as spaces. What passes there and fails in fux-vt is what fux-vt
  lacks.
- **Another engine** (`--terminal NAME`, any engine in this process): the
  engine is the terminal (`src/answering.rs`), and its replies go back in
  order. No engine but fux-vt answers DECRQCRA, so it is answered from the
  engine's snapshot with xterm's checksum. In origin mode, where the
  rectangle is relative to margins no snapshot gives, the test is skipped.
  Ghostty is given its size as Ghostty's app gives it. The results are
  checked against `esctest-expected-NAME.txt`.
- **Beside** (`--beside NAME`, fux-vt or an engine): runs another terminal
  too, checked against its own list, and compares the two: each area's
  pass rate, and the tests one passes and the other fails, each with its
  feature and where that feature is specified (`SOURCES` in
  `src/esctest.rs`).

`--json FILE` writes, for the scoreboard:

- the run: `suite`, `expected_terminal`, `max_vt_level`, `timeout`,
  `filter`, `tests`, and `terminal` with `--terminal`;
- a column per place:
  - `direct`, plus `in_fux` and `xterm` with those flags, and one named for
    the `--beside` terminal (`fux_vt`);
  - each with `passed`, `passed_beyond_xterm`, `failed`, `skipped`,
    `pass_rate` (passed over passed and failed), `seconds`, per-area
    counts in `areas`, and each test's `outcome`, `message` and `listed`
    reason in `tests`;
- `differences_in_fux` and `differences_xterm`;
- with `--beside`, `comparison`: `terminals`, `total`, `areas`,
  `passes_only_in` (each with `test`, `kind`, `feature`, `source`,
  `failure`) and `run_in_one_only`;
- `mismatches`: what failed the run.

The pass rates are in [`scoreboard/SCOREBOARD.md`](scoreboard/SCOREBOARD.md).

## The input encoders

`encoders` holds fux-vt's input encoders (`fux_vt::keys`) beside
libghostty-vt's. Both terminals are put in the same modes by the same bytes:
every mouse tracking mode in every encoding, focus reporting and bracketed
paste on and off, normal and application cursor keys with modifyOtherKeys 2
and each kitty flag set 1 to 31. Then each encoder reads the modes from its
own terminal (`Screen::encode_key`, `encode_mouse`, `encode_focus`,
`encode_paste`; ghostty's `set_options_from_terminal`) and the bytes are
compared:

- keys: Enter, Tab, Escape, Backspace, Delete, Insert, Home, End, the page
  keys, the arrows, F1 to F12 and what a US layout types, each with every
  combination of Shift, Alt and Ctrl, as ghostty's host makes the event (the
  text the key types, its unshifted key, Shift consumed by a shifted
  character) and as fux-vt's decoder reads a kitty-protocol terminal's
  report of it;
- mouse: every button pressed, released and dragged, and motion with none,
  with every modifier, at positions about each encoding's limits (94/95,
  222/223, 2014/2015);
- focus changes, and pastes: a few fixed texts (nested end markers, the C1
  end) and `--cases` random ones (default 2000) from pieces that include
  controls, newlines and end markers.

A difference fails `encoders` unless it is one of its recorded verdicts,
each with the source that decides it, printed with every run:

| Verdict | Decided by |
| --- | --- |
| `utf8-button`, `utf8-limit` | ctlseqs: in UTF-8 mouse mode Cb is UTF-8 encoded too, and 2015 is the last position; ghostty writes Cb raw and has no limit |
| `legacy-extras`, `f3`, `modify-other-keys` | xterm: with no keyboard mode a program gets legacy bytes (ghostty sends CSI u and CSI 27 forms, and kitty's F3); at modifyOtherKeys 2 every modified key but Shift with a printable is CSI 27 ; m ; k ~ |
| `kitty-flags-without-disambiguate`, `kitty-press-type` | the kitty spec: it gives no other encoding without flag 1 or 8, and lets a press leave out its event type, as kitty does |
| `ctrl-backspace` | a choice: fux-vt's Ctrl-Backspace is DEL, ghostty's BS |
| `paste-controls`, `paste-newline` | a choice: ghostty, as xterm, makes NUL, BS, ENQ, EOT, ESC, DEL and the tty's special characters spaces, and an unbracketed LF a CR; fux-vt passes a paste as pasted, removing only what would end its bracket |
| `paste-c1-end` | fux-vt: U+009B 201 ~ ends a bracket for a terminal that reads C1, so fux-vt removes it as it removes ESC [ 201 ~ |

It needs no engine but ghostty, takes a few seconds, and is part of
`run.sh quick`.

## Speed

`bench` feeds each engine every workload in 4 KiB chunks, on a 50×200
screen with 10000 rows of history, and prints MB/s, the best of three
runs. The synthetic workloads follow alacritty's vtebench: ascii,
dense-cells, medium-cells, cursor-motion, scrolling, scroll-region and
unicode. `corpus` is every recording in turn, at the first one's size
(40x120). `bench corpus` adds each recording alone (`corpus:vim` and so
on) at the size it was recorded at, without its resizes. `--mb N` sets
the MiB per workload (8). An engine in its own process also pays for
the pipe, so its figure is end to end and marked `*`.

`bench --instructions` counts instructions retired per byte instead,
because load doesn't move them, for every engine in this process. Each
engine and workload runs in a child process (`src/count.rs`: `/usr/bin/time
-l` on macOS, `perf` or cachegrind on Linux), less a baseline child that
does everything but the feeding. The figure is the fewest of `--repeats`
runs (3); the spread is its noise. With no workloads named it counts the
synthetic ones and the corpus. `--jobs N` and `--json FILE` work as in
`footprint`.

## Memory

`footprint` measures the memory each engine in this process holds
(`--engines` picks), each measure in a child process of its own. The child
makes the bytes, reads its memory, makes and feeds the engine, and reads its
memory again. The difference is counted two ways (`src/memory.rs`):

- **footprint**: dirty memory, resident or compressed (`phys_footprint` on
  macOS, `RssAnon` on Linux). It sees every allocator, including Ghostty's
  own page mapping.
- **malloc**: bytes handed out and not returned (`malloc_zone_statistics`,
  macOS only). It is exact, but blind to memory mapped directly.

At 50 rows and 80 and 200 columns, it measures:

- an empty screen;
- a screen of `dense-cells` and one of `medium-cells`;
- 10,000 rows of history from each synthetic workload and from the corpus.

Every engine keeps 10,000 rows of history, except Ghostty: its limit is in
bytes, so it gets 1 GiB and keeps every row. The figure per row is the
footprint beyond the same engine's empty screen, over the rows it holds.
`--json FILE` writes every measure; `--jobs N` sets how many children run
at once.

## The scoreboard

`run.sh scoreboard` gathers the last results in `target/harness` into
`scoreboard.json` and `scoreboard.md` there. It also keeps them in the
repository:

- [`scoreboard/SCOREBOARD.md`](scoreboard/SCOREBOARD.md): the page for
  the last commit;
- `scoreboard/history.jsonl`: one line per commit, replacing the last line
  if it is the same commit's.

Commit both with the work they measure. It runs nothing itself. Any axis
whose check has not run says `not run`.

The axes:

- conformance: esctest's pass rate, directly and in a fux pane;
- real programs: corpus recordings agreeing;
- the multiplexer: transparency, beside tmux and zellij;
- speed: instructions against main, and MB/s beside Ghostty and alacritty;
- latency;
- footprint and bytes per frame;
- robustness: fuzz time since the last crash;
- fux-vt beside Ghostty's core, axis by axis;
- each check's stamp.

## Files

| File | What |
| --- | --- |
| `run.sh` | fetches and pins Zig, Ghostty and libvterm, installs node deps, builds, runs |
| `harness.sh` | `quick`, `full`, `deep`, `fuzz` and `scoreboard`, run by `run.sh` |
| `build.rs` | compiles libvterm and its shim |
| `src/main.rs` | the commands |
| `src/engine.rs` | the `Engine` trait and its reading rules, `Can`, `Blanks`, the engine list |
| `src/engines/*.rs` | one adapter per engine, documenting its quirks; `pane.rs` is shared by tmux and xterm |
| `node/` | the xterm.js server and its pinned packages |
| `src/case.rs` | running, voting, generating and shrinking cases; reports |
| `src/families.rs` | the families: generators, statuses and reasons (also included by `diff/oracle`) |
| `src/cases.rs` | the named cases |
| `src/snapshot.rs` | what is compared, field by field, and the side-by-side view |
| `src/rng.rs` | splitmix64, as in `diff/`; also included by `diff/oracle` |
| `src/escape.rs` | bytes as replayable text, and back |
| `src/record.rs` | `record` |
| `src/corpus.rs` | the recordings: loading, replaying, their statuses |
| `src/inventory.rs` | `inventory` |
| `src/transparency.rs` | `transparency`, with `KNOWN` and the multiplexers |
| `src/esctest.rs` | `esctest`, and comparing two terminals |
| `src/encoders.rs` | `encoders`, with its recorded verdicts |
| `src/answering.rs` | an engine as the terminal a program talks to, for `esctest --terminal` |
| `src/bench.rs` | `bench`: the workloads and the speed table |
| `src/instructions.rs`, `src/count.rs` | `bench --instructions`, counting a child's instructions as `bench/src/count.rs` does |
| `src/footprint.rs`, `src/memory.rs` | `footprint`, and the process's memory as the system and malloc count it |
| `src/scoreboard.rs` | `scoreboard` |
| `esctest-expected.txt`, `esctest-expected-ghostty.txt` | the esctest tests fux-vt and Ghostty's core fail, each with its reason |
| `corpus/` | the recordings, their keys, `record.sh`, the man pages they show, and `INVENTORY.md` |
| `scoreboard/` | the kept scoreboard and its history |
