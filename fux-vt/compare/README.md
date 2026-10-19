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

It needs Zig, a Ghostty checkout and libvterm's source, which `run.sh`
fetches, and, for the engines in their own processes, Node, tmux, xterm
and Xvfb. Built without its default feature `engines`
(`cargo build --release --no-default-features`), it links no other engine
and needs only cargo: what needs none runs (`esctest` against fux-vt), and
the others say they were not built. CI runs that `esctest` on every PR;
`compare.yml`, on request, runs the rest beside every engine.

## Running it

`run.sh` sets up the environment, builds, and runs `fux-vt-compare` with
its arguments, or one of the harness commands in `harness.sh`:

| Command | When | Budget | Runs |
| --- | --- | --- | --- |
| `run.sh quick` | before a commit | 60 s | `corpus` beside xterm and the panel; `corpus --subject ghostty`; `transparency`; `run --cases 2000`; `cases`; `encoders`; the oracle (`diff/oracle.sh`, see [`diff/README.md`](../../diff/README.md)) |
| `run.sh full` | before a PR | 600 s | `quick`; `run --cases 20000` with and without `--no-reflow`; `esctest`; `esctest --terminal ghostty --beside fux-vt`; `fux-bench --against REF`; `footprint`; `bench --instructions` |
| `run.sh deep` | before a release, or when hunting | none (prints an estimate) | `full`; 20,000 random cases at the day's seed (`run --seed YYYYMMDD`, printed; seed 1 stays the gate); `verdicts` for seeds 1–20 (`FUX_DEEP_SEEDS`); `esctest --in-fux`, `--xterm`, and `--terminal E --unlisted` for every other engine; `transparency --multiplexers`; `fux-bench feel` and `info`; 10 minutes of fuzzing |
| `run.sh fuzz [MINUTES]` | by hand | MINUTES (10) | the oracle's pin, then each fuzz directory built on its own, then every target `cargo fuzz list` names in those that built, sharing the time; a crash is minimized (`cargo fuzz tmin`) and listed, to be made a test |
| `run.sh scoreboard` | after any of them | seconds | the last results, gathered (see "The scoreboard") |

Each takes `--jobs N` (at most N checks at once, a check with threads of
its own alone with N, and N cargo jobs) and `--against REF` (what the
oracle and the instruction counts compare with; the latest master by
default). Each ends with a summary of its checks and exits 1 if any
failed, or says so if it ran over its budget. Independent checks run side
by side, while whatever times itself or counts instructions runs alone.

Every required check is in [`manifest.toml`](manifest.toml): its command,
what it needs, and its schedule, including those nothing runs yet. A
command also fails when a check the manifest schedules for it did not run,
and when it ran one the manifest does not list.

Results (`NAME.json`), logs (`logs/NAME.log`), a stamp for each check
(`NAME.stamp.json`: the product and harness commits, each the last
commit that changed fux-vt and fux, or the tools, and `-dirty` if their
files differ from it; the binary's SHA-256, the exact command, when, how long, the
exit status) and the fuzz ledger (`fuzz.jsonl`) go to
`fux-vt/compare/target/harness`, or `$FUX_HARNESS_OUT`. A check's earlier
results and stamp are removed before it runs. `FUX_PRODUCT_COMMIT` names
the product when it is not HEAD's (fux-vt at another commit, by a path
override). The binaries are run from `CARGO_TARGET_DIR` when it is set.

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
report says why, and the run goes on. An abstention has a class
(`engine::abstention`): *adapter-limited*, where the adapter cannot read
what it was given (its error begins `cannot tell:`, as xterm's for SGR
58), *engine*, a panic, or *infrastructure*, anything else (a reply or a
print that never came). Every engine a LIST means must be able to run
here, so a check is judged by the same engines wherever it runs;
`--missing-ok` lets `panel`, `all` and `in-process` leave one out. A
decided family's named case fails when none of its deciders gives a
verdict, unless the family is a standard's and they could not read the
case (an adapter's limit): its section's expected fields judge it then.

**The default panel** is every voter that runs here: ghostty, alacritty,
libvterm, avt and wezterm, all in this process and fast.

- **vt100 doesn't vote.** It is fux-vt's ancestor, and its inherited
  choices are what the vote is meant to catch.
- **xterm.js, tmux and xterm don't vote by default.** They run in
  processes of their own, about 20 times slower. Add them with `--engines
  panel,xterm.js` and so on. `cases` runs beside every engine that votes
  (`panel,xterm.js,tmux,xterm`: all but vt100), and judges every step of
  a case, not its end alone.
- **The panel can be wrong.** Where it splits, the standards and xterm
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

The engines run in this process, so one that grows without end would
take the machine: alacritty_terminal 0.26's resize loops forever
narrowing a wide glyph's row to one column (`replay --engines alacritty
--size 1x2 --history 0 '\u{4e00}' resize:1x1` took 55 GB on 2026-10-17).
Its adapter refuses any shrink to one column, so alacritty abstains
there, an engine defect. And `fux-vt-compare` limits its own data
(`RLIMIT_DATA`, which its children inherit) to 4 GiB, or the bytes
`FUX_VT_COMPARE_DATA_LIMIT` gives (0: no limit of its own), so anything
else of the kind fails an allocation instead.

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
| xterm | the installed xterm (patch 397 or later, for `printRawChars`; CI builds 411) under one Xvfb per run, read by printing every page through `printerCommand` | on request | decides most disputed families (`verdicts`) and the corpus; abstains from a case with SGR 58, which it lacks |

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
  engine's most recent rows;
- the device's state DA claims (HAR-46), where an engine tells it: the
  cursor's shape (DECSCUSR's Ps; fux-vt's own, the terminal's, is compared
  with no one), the tab stops, each screen row's line rendition, and the
  cells DECSCA or SPA protects. fux-vt's adapter reads all four (the stops
  by HT and the protected cells by what DECSED leaves, on copies); xterm's
  the first three (DECRQSS, DECTABSR and its print's `ESC # n`); the other
  engines none.

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
- **The history rows a case asks for are compared, or all fux-vt keeps,
  the more.** Engines keep far more than a case can fill. fux-vt's budget
  is in bytes: it gets the case's rows at 24 bytes and 8 a cell, and 64
  KiB beside them for its maps and styles, so a subject keeping fewer
  rows than the others differs on "history rows kept", as one keeping
  more does. Cases that resize give fux-vt 10000 rows, since a screen
  that grows pulls rows back from history.
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
- **Decided** (`Status::Decided`): the engines split, and a source
  settles it (`authority`): a standard's section where it speaks
  (`Authority::Standard`), else xterm 411 (`Authority::Xterm`), as
  fux-vt's [README](../README.md) orders them. Its reason (`why`) says
  where the engines stand, and `by` names the engines it is checked
  beside: xterm for the VT families, and for a feature xterm lacks (OSC
  133), the engines that follow the feature's spec. A family a standard
  decides names the fux-vt test whose doc comment cites the section;
  `by` is empty where no engine follows the standard all through (on
  screens of one or two columns xterm departs from DEC STD 070), and the
  test is then the check. `verdicts` runs a standard's test alone (`cargo
  test --exact`), for every family it decides, and the test must pass.
  Each named case of such a family has its expected fields
  (`cases::EXPECTED`): what the section settles, by the key a difference
  names (`row 0`, `cursor`, `pending wrap`, `cell (0,1) bold`), with the
  section, its edition and the quoted passage the values were derived
  from by hand. They judge those fields whatever the engines do, and `by`
  judges only the rest, so an engine that departs from the section never
  outvotes it, and a family with no engine beside it is still compared.
  A family xterm decides is checked by xterm agreeing. One whose check
  can't be made (the test missing, citing something else or failing, a
  case without its expected fields, xterm not among `by` or not run)
  fails. The default panel's majority is often the side the source
  decides against, so `run` leaves these out. `verdicts` runs each
  family's named cases, then random cases from it, beside `by` (the named
  cases alone where `by` is empty). `cases` fails a named case of a
  decided family where it departs from its expected fields, or those
  engines outvote fux-vt on the rest.
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
  scenario does;
- its line in `corpus/SHA256SUMS`, which a test checks the bytes against;
  `record` writes it, and, in the JSON, when, from which commit and on
  what system the recording was made (`recorded`: the 113 recorded before
  it have none).

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
4. Give NAME its exceptions in `STATUSES` (`src/corpus.rs`), none if it
   agrees. A recording without a status fails `corpus`.
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
  abstains for a limit of its adapter's (from SGR 58 on). A field the
  panel outvotes fux-vt on while xterm agrees with fux-vt is shown in the
  marks (`-alacritty`), not failed. If xterm abstains for anything else
  (infrastructure), the recording is *unjudged*, and the run fails: the
  vote alone would judge the rest. Each recording's abstentions, with
  their classes, are in the JSON.
- **A resize is judged by the program's answer to it.** A resized step is
  two steps of the case: the resize, then the program's output. Only the
  second is judged, because right after a resize each engine shows its
  own way of resizing. fux's panes reflow, as Ghostty, wezterm and libvterm
  do; xterm doesn't.

Each recording has its exceptions (`STATUSES` in `src/corpus.rs`): none
where it is expected to agree, else each difference it is expected to
show, by the recorded steps it shows at (`after step N`, as a report
counts them), the fields (a difference's key, `*` for any run of
characters: `cell (*) text`, `title`) and the reason, with a command that
shows it. The comparison goes on past a covered difference to the
recording's end. `corpus` exits 1 on a difference no exception covers, on
an exception that no longer occurs, and on a recording with no status;
`corpus NAME` lists each covered difference with its step.

The replies a recording kept, what fux-vt answered the program as it ran,
are compared too: the recording is replayed through a parser set up as
`record` set it up, and fux-vt's replies now must be the ones kept, with
version numbers aside (XTVERSION's, DA2's) and each change made since it
was recorded applied (`REPLY_CHANGES`, each cited; one no recording shows
any more fails). A recording whose replies changed otherwise is listed in
`REPLIES_DIFFER` with why, and fails once it agrees again. So a change to
DA, DECRQM or the colour replies shows in the corpus.
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
fux-vt/compare/run.sh esctest --terminal alacritty --unlisted       # any engine in process, scored with no list
fux-vt/compare/run.sh esctest --tmux             # also in a real tmux, held to no list, as xterm is
```

- **Directly** (`src/esctest.rs`): each test is one esctest process on a
  25×80 PTY, on a terminal made for it alone: every terminal (fux-vt, an
  engine, a fux pane, xterm) is isolated the same way, so no test starts
  from what an earlier one left (esctest's own reset before each test still
  runs). Its terminal is a fux-vt parser set up as fux's panes are, plus
  rectangle checksums and DECXCPR (`ExtendedReplies`), which panes leave
  off. Tests run in parallel, up to `--jobs` (one per CPU by default), and
  are reported by area (a test class). A read waits `--timeout` (1 s) for
  its reply, so a missing report fails only that test. A test past
  `--limit` (60 s) is stopped.
- **Options passed to esctest:** `--expected-terminal=xterm` (fux-vt
  follows xterm), `--max-vt-level=4` (a VT420, xterm's default and the
  level with DECRQCRA), `--xterm-reverse-wrap=383` (reverse wraparound as
  xterm has it since patch 383), and no `--options` for fux-vt and the
  engines (UTF-8, no window operations for programs). Real xterm's column
  gets `--options xtermWinopsEnabled`, with every window operation
  allowed and `precompose: false`, as esctest's README sets it up. VT520
  tests are skipped. esctest2 is
  pinned at 2798f121, and a checkout at another commit is refused.
- **One denominator:** every terminal is scored over the tests esctest
  runs (it skips the same ones for all). Each result is a pass; a failure
  (the terminal's: an assertion, a report it did not give, or the engine's
  own error); or unavailable, neither: *adapter-limited*, where the
  harness cannot read the cells, or *infrastructure*, where its run gave no
  result (a test stopped at `--limit`), which fails the run without
  counting against the terminal. A pass or a failure is *native*, or
  *adapter-observed* where the harness answered a report for the engine
  in that test (below).
- **The list**, `esctest-expected.txt`: every failing test with its reason,
  one of `departure`, `spec`, `not-implemented`, `xterm-too` or `bug` (the
  file's header defines them). The run fails on an unlisted failure and on
  a listed test that passes, so the list never goes stale. A line tagged
  `[fux]` fails only in a pane, and `[direct]` only directly.
- **In fux** (`--in-fux`): builds fux and runs each area in the only pane
  of a fux server of its own (under `fux-esctest-*` in `$TMPDIR` if it is
  short, else `~/.cache/fux`; never the user's), with a client on a 26×80
  PTY. The pane runs esctest as the server's `SHELL`: `fux __serve` takes
  no config, and a client's `config.fux` reaches only panes made after the
  first. Panes don't answer DECRQCRA, so tests that read cells are
  unavailable. A test that passes in one place and fails in the other is
  printed, and points at fux.
- **In xterm** (`--xterm`): each area also runs in a real xterm under Xvfb,
  set up as esctest's README says, with DECRQCRA allowed and
  `checksumExtension: 26` (empty cells counted as spaces, no attributes,
  each cell's own character: without bit 4, xterm 411 sums a per-cell copy
  that DCH, ICH and the scrolls never move, `screen.c:3209`). What passes
  there and fails in fux-vt is what fux-vt lacks.
- **Another engine** (`--terminal NAME`, any engine in this process): the
  engine is the terminal (`src/answering.rs`), and its replies go back in
  order. No engine but fux-vt answers DECRQCRA, so it is answered from the
  engine's snapshot with xterm's checksum; the size report esctest asks
  before every test (`CSI 18 t`) is answered from the snapshot too where the
  engine gives none (libvterm, avt, vt100). Either makes the test
  adapter-observed. In origin mode, where the rectangle is relative to
  margins no snapshot gives, the test is adapter-limited; for an engine
  whose origin mode cannot be read (vt100), from the program's DECSET 6 to
  its DECRST 6, DECSTR or RIS. Ghostty is given its size as Ghostty's app
  gives it. The results are checked against `esctest-expected-NAME.txt`.
- **Beside** (`--beside NAME`, fux-vt or an engine): runs another terminal
  too, checked against its own list, and compares the two: each area's
  pass rate, and the tests one passes and the other fails, each with its
  feature and where that feature is specified (`SOURCES` in
  `src/esctest.rs`).

`--json FILE` writes, for the scoreboard:

- the run: `suite`, `suite_commit`, `expected_terminal`, `max_vt_level`, `timeout`,
  `filter`, `tests`, and `terminal` with `--terminal`;
- a column per place:
  - `direct`, plus `in_fux` and `xterm` with those flags, and one named for
    the `--beside` terminal (`fux_vt`);
  - each with `passed`, `passed_beyond_xterm`, `failed`, `skipped`,
    `unavailable` (`adapter_limited`, `infrastructure`), `run` (the
    denominator: all but the skipped), `pass_rate` (passed over run),
    `adapter_observed`, `seconds`, per-area counts in `areas`, and each
    test's `outcome` (`pass`, `fail`, `skip`, `unavailable`), its `basis`
    (`native` or `adapter-observed`), its `class` (`engine` for a failure,
    `adapter-limited` or `infrastructure` for an unavailable result),
    `message` and `listed` reason in `tests`;
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
own terminal (`Screen::key_mode` for `key_bytes`, `encode_mouse`,
`focus_report`, the bracketed paste mode for `paste`; ghostty's
`set_options_from_terminal`) and the bytes are
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
| `legacy-extras`, `f3`, `modify-other-keys` | xterm: with no keyboard mode a program gets legacy bytes (ghostty sends CSI u and CSI 27 forms, and kitty's F3); at modifyOtherKeys 2 every modified key is CSI 27 ; m ; k ~, Shift-letters too, but Ctrl alone on Backspace and Shift alone on a character Ctrl makes no control of (`!`, `)`, `+`), which types its text |
| `ctrl-alt-question`, `shift-tab`, `ctrl-backspace-bs` | real xterm 411's key bytes (KEY-13, `tests/data/keys/xterm-keys.txt`): Ctrl-Alt-? is ESC ?, where ghostty sends ESC DEL; Shift-Tab at modifyOtherKeys 2 is CSI Z, where ghostty sends CSI 27 ; 2 ; 9 ~; Backspace with Ctrl and another modifier is BS (ESC BS, or CSI 27 ; m ; 8 ~ at modifyOtherKeys 2), where ghostty sends DEL |
| `kitty-flags-without-disambiguate`, `kitty-press-type` | the kitty spec: it gives no other encoding without flag 1 or 8, and lets a press leave out its event type, as kitty does |
| `paste-controls` | a choice: ghostty, as xterm, makes NUL, BS, ENQ, EOT, ESC, DEL and the tty's special characters spaces; fux-vt passes them as pasted, removing only what would end its bracket |
| `paste-newline` | xterm 411 (`button.c`): each LF of a paste is sent as CR, bracketed too, as fux-vt sends it; ghostty keeps a bracketed paste's LF |
| `paste-c1-end` | fux-vt: U+009B 201 ~ ends a bracket for a terminal that reads C1, so fux-vt removes it as it removes ESC [ 201 ~ |

It needs no engine but ghostty, takes a few seconds, and is part of
`run.sh quick`.

## Speed

`bench` feeds each engine every workload in 4 KiB chunks, on a 50×200
screen with 10000 rows of history, and prints MB/s, the median of three
rounds, each round taking every engine in turn (so a machine that drifts
moves them alike; pin it with `taskset` to keep it on one set of cores,
which the heading records). The synthetic workloads follow alacritty's vtebench: ascii,
dense-cells, medium-cells, cursor-motion, scrolling, scroll-region and
unicode. `corpus` is every recording in turn, at the first one's size
(40x120). `bench corpus` adds each recording alone (`corpus:vim` and so
on) at the size it was recorded at, without its resizes. `--mb N` sets
the MiB per workload (8). An engine in its own process also pays for
the pipe, so its figure is end to end and marked `*`.

`bench --instructions` counts instructions retired per byte instead,
because load doesn't move them, for every engine in this process. Each
engine and workload runs in a child process (`bench/src/count.rs`, fux-bench's counter, built here too: `/usr/bin/time
-l` on macOS, `perf` or cachegrind on Linux), less a baseline child that
does everything but the feeding. The figure is the median of `--repeats`
(3) paired rounds, each run less its round's baseline, and a round at or
below its baseline fails it; the widest round's deviation is its noise. With no workloads named it counts the
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

## The conformance ledger

`ledger.json` is one table of checks from every conformance suite that
runs: each esctest2 test (567, 18 of them skipped by esctest), each named
case (`cases`) and each corpus recording. Each check carries:

- its **feature**, one of the frozen groups in `features` (an esctest
  area's function, with the 18 colour-space tests as one feature and every
  reverse-wrap test as one choice; a named case's family; a recording's
  program);
- its **authority**, an id in `authorities`: a standard's document,
  edition, section and quoted text, with the levels the document gives the
  function; xterm's ctlseqs at patch 411 where the standards are silent;
  or `open` where none is known (never "engine-specific", never out of
  scope);
- its **level and mode** (esctest's `@vtLevel`, 7-bit or 8-bit controls,
  options it needs), and its **applicability** (`applies`, `n/a: WHY`, or
  `skipped: WHY` where the suite does not run it);
- its **verdict**, how the suite's expectation stands to the authority:
  `agrees`, `departs` (the suite expects what the text contradicts),
  `allows` (the text also allows other answers), or `open` (not read);
  with `follows` or `follows_when` saying which engines do what the text
  says where the suite does not;
- the **evidence** for each tag: the area's function, the test's own
  decorators, its listed reason, `cases::EXPECTED`, a family's authority,
  or a reading of the test against the quoted text.

`fux-vt-compare ledger --results DIR` scores every engine whose results
are in `DIR` (esctest's `--json` columns, `cases --json` and `corpus
--json`, one file a subject), on one denominator a suite, four ways:

| Score | What it counts |
| --- | --- |
| raw | the checks passed, as the suite reports, over the checks it runs |
| standard-adjusted | each check scored by its verdict, over the checks that apply: a `departs` check passed is a fail |
| xterm-compat | the checks passed among those real xterm passes, xterm's own column left out (the corpus is judged by xterm wherever xterm can tell a field, so there it is the raw score) |
| feature | the features all of whose applying checks pass |

A check an engine has no result for is unavailable: in the denominator,
never a pass, and counted apart. `--table FILE` writes the whole ledger
as one Markdown table. `run.sh deep` runs the subjects and the ledger.
The tags were written by `<notes>/ledger-tools/gen.py` from the area
tables, esctest's decorators and the lists; from checkpoint 0 on, the
data file is the record, and a retag shows in the list-diff report.

## The lists and the list-diff report

Every list a comparison judges by is also data: `fux-vt-compare lists
--json FILE` writes them as one JSON, each a map from an entry's id to
the entry with its `citation` (the reason, section or quote it rests on):

| List | From |
| --- | --- |
| `esctest-expected`, `esctest-expected-NAME` | `esctest-expected*.txt`: scope, kind, reason |
| `corpus-statuses` | `corpus::STATUSES` |
| `transparency-known` | `transparency::KNOWN`, by name |
| `families` | `families::FAMILIES`: status, `by`, authority, reason |
| `cases`, `expected-fields` | `cases::CASES` and `cases::EXPECTED` |
| `encoder-verdicts` | `encoders::VERDICTS` |
| `ledger` | `ledger.json`'s tags, by `SUITE:ID`: feature, authority and its section, verdict, applicability |

`run.sh quick` writes them to `lists.json`. Checkpoint 0 freezes them as
`lists-c0.json`; from then on `fux-vt-compare lists --against
lists-c0.json [--esctest DIR]` prints, as Markdown for the PR, every
entry added, removed or changed beside its line in `list-changes.txt`
(`LIST ID CITATION`), and fails on a change with no line there or a line
with no change. For a test added to an esctest list it names the engines
whose esctest JSON in `DIR` has it passing.

## The scoreboard

`run.sh scoreboard` gathers the last results in `target/harness` into
`scoreboard.json` and `scoreboard.md` there. It also keeps them in the
repository:

- [`scoreboard/SCOREBOARD.md`](scoreboard/SCOREBOARD.md): the page for
  the last commit;
- `scoreboard/history.jsonl`: one line per commit, replacing the last line
  if it is the same commit's.

Commit both with the work they measure. It runs nothing itself. Any axis
whose check has not run says `not run`, and one whose check failed says
so: a check's results are read only beside its stamp, and only if it
passed. It exits 1 when the stamps name more than one product commit: a
column is one product's.

The axes:

- conformance: esctest's pass rate, directly and in a fux pane;
- real programs: corpus recordings agreeing;
- the multiplexer: transparency, beside tmux and zellij;
- speed: instructions against master, and MB/s beside Ghostty and alacritty;
- latency;
- footprint and bytes per frame;
- robustness: fuzz time since the last crash;
- fux-vt beside the best other engine, row by row (esctest's passes on one
  denominator, the corpus beside Ghostty, instructions per byte, memory,
  MB/s): ahead or behind only past the row's floor, a tie within it, and
  unranked with no floor. The floors: exact for counts; the spread of the
  repeats for instructions, or `floors.json`'s `instructions` share (in
  percent) when measured apart; two pages over the divisor for memory (a
  figure whose whole is under a page is under resolution); and
  `floors.json`'s `mb_per_s` for MB/s, which is unranked without it;
- each check's stamp.

## Files

| File | What |
| --- | --- |
| `run.sh` | fetches and pins Zig, Ghostty and libvterm, installs node deps, builds, runs |
| `harness.sh` | `quick`, `full`, `deep`, `fuzz` and `scoreboard`, run by `run.sh` |
| `lib.sh` | the harness's step, spec and together, which `harness.sh` sources |
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
| `src/instructions.rs` | `bench --instructions`, counting a child's instructions by fux-bench's own counter (`bench/src/count.rs`, built into this crate) |
| `src/footprint.rs`, `src/memory.rs` | `footprint`, and the process's memory as the system and malloc count it |
| `src/scoreboard.rs` | `scoreboard` |
| `src/lists.rs` | `lists`: every judged list as JSON, and the list-diff report |
| `src/ledger.rs`, `ledger.json` | `ledger`: the conformance ledger's tags, and its four scores |
| `list-changes.txt` | each change to a judged list since checkpoint 0, with its citation |
| `esctest-expected.txt`, `esctest-expected-ghostty.txt` | the esctest tests fux-vt and Ghostty's core fail, each with its reason |
| `corpus/` | the recordings, their keys, `record.sh`, the man pages they show, and `INVENTORY.md` |
| `scoreboard/` | the kept scoreboard and its history |
