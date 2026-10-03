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
workloads. The corpus (`corpus/`) holds what real programs wrote to a
terminal, recorded by `record`.

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
fux-vt/compare/run.sh verdicts              # the families with a recorded verdict, beside the engines that decide each
fux-vt/compare/run.sh matrix                # family by engine: % of cases each engine differs from fux-vt
fux-vt/compare/run.sh bench                 # MB/s for every engine on every workload, and the corpus
fux-vt/compare/run.sh bench --engines ghostty corpus   # each recording alone too
fux-vt/compare/run.sh --list                # the families, their status and reasons
fux-vt/compare/run.sh survey                # each family alone: how often it fails, and the smallest failure
fux-vt/compare/run.sh run --family sgr --family text --cases 2000 --seed 7
fux-vt/compare/run.sh run --engines ghostty,libvterm,xterm   # any panel
fux-vt/compare/run.sh replay --engines all --size 1x5 'abcde\x08X'
fux-vt/compare/run.sh --no-reflow           # fux-vt set up as fux sets it up
fux-vt/compare/run.sh corpus                # the recordings beside xterm and the panel; exit 1 if one regresses
fux-vt/compare/run.sh corpus --show vim     # one recording, and fux-vt's screen at its end
fux-vt/compare/run.sh inventory > fux-vt/compare/corpus/INVENTORY.md   # what the recordings send
fux-vt/compare/corpus/record.sh [NAME...]   # record the corpus again (see "The corpus")
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
| fux-vt | the subject, by path, set up as ratty sets it up (reflow, an identity, events), with the DECRQM answers, in-band resize, colour-scheme reports, kitty keyboard, hyperlinks and prompt marks fux's panes have | — | `--no-reflow`: as fux sets it up |
| ghostty | libghostty-vt 0.2.1 over Ghostty `7aa95917`, built by Zig 0.16 (see "Setup") | link groups | mode 2027 on; history kept in bytes; a link's URI only (`GridRef::hyperlink_uri`), a row's mark from `Row::semantic_prompt` (a primary prompt's row, not a continuation's) |
| alacritty | alacritty_terminal 0.26.0 | blink, 2026, 2048, prompt marks | synchronized updates applied at once (no event loop); a wide glyph on one column panics it, which the adapter repairs |
| libvterm | libvterm 0.3.3 from its release tarball, through a C shim (`src/engines/libvterm_shim.c`); modes and pending wrap read from the pinned source's `vterm_internal.h` | dim, underline colour, kitty, 2026, 2048, links, prompt marks | the shim guards five crashes, hangs and out-of-bounds reads that random cases reach (found with ASan and UBSan; each listed with a replay in its file) |
| avt | avt 0.18.0 | underline colour, hidden, keypad, bracketed paste, focus, kitty, 2026, 2048, links, prompt marks, title, reports | takes `&str`: the adapter carries split UTF-8 and turns invalid bytes into U+FFFD |
| wezterm | wezterm-term at `cab25161` (git) | pending wrap, 2026, 2048, link groups, prompt marks | replies come through a writer thread, synced with a paste marker; links without an id to one URI are one link |
| vt100 | vt100 0.16.2 | underline colour, blink, hidden, strikeout, autowrap, origin, focus, kitty, 2026, 2048, links, prompt marks, reports | doesn't vote |
| xterm.js | @xterm/headless 6.0.0 with addon-unicode-graphemes 0.4.0, `reflowCursorLine` on, one Node process for every terminal (`node/engine.mjs`) | underline colour, kitty, 2048, prompt marks | patches a crash in ED 1 (see its file); links read from its core, not the public API (`urlId`, `OscLinkService`) |
| tmux | the installed tmux (3.7c here): a private server, one session per terminal, read with `capture-pane -p -e -N -F` and `display -p` | kitty, 2026, 2048, link groups | links from the OSC 8 `-e` prints, prompt marks from `-F`'s `P`; a `sh` pane program copies bytes in raw mode; every step is synced with DA1 (`CSI c`), which no family sends; a sync waits for a reply to each DA1 request in the output too (real programs send them) |
| xterm | the installed xterm (XTerm 411 here) under one Xvfb per run, read by printing every page (`CSI ? 11 i`) through `printerCommand`, modes by DECRQM, resize by `CSI 8 t` | pending wrap, underline colour, kitty, 2026, 2048, links, prompt marks | the deciding vote for most disputed families (`verdicts`); the style of a row's blank cells after its last drawn cell cannot be read, and xterm abstains from a case with SGR 58, which it lacks (see its file) |

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
  wide, wide tail), style (foreground, background and underline colour;
  bold, dim, italic, underline of any style, blink of either speed, inverse,
  hidden, strikeout) and hyperlink (OSC 8): its URI (fux-vt, Ghostty,
  alacritty, wezterm, xterm.js and tmux can tell), and which cells share a
  link (fux-vt, alacritty and xterm.js). Engines name links their own ways,
  so the names are not compared: among the cells both engines link to the
  same URI, each cell's link is named by the first of them that has it;
- each row's soft-wrap flag, and whether a prompt starts on it (OSC 133 A:
  fux-vt, Ghostty and tmux can tell);
- the cursor, and whether a wrap is pending (a cursor waiting to wrap is in
  the last column with `pending_wrap` set);
- cursor visibility, DECAWM, DECOM, the alternate screen, DECCKM, DECKPAM,
  bracketed paste, focus reporting, synchronized output (2026: fux-vt,
  Ghostty and xterm.js can tell), in-band resize (2048: fux-vt and Ghostty),
  and the kitty keyboard flags;
- the title;
- cursor position and status reports (`CSI r;c R`, `CSI 0 n`);
- history: the text and wrap flag of every row fux-vt keeps, against the
  engine's most recent rows.

What is normalized away, and why:

- **A printed space and an empty cell are both blank.** Engines store spaces
  differently.
- **The cell after a wide glyph has no text, style or link of its own.**
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
  - no empty kitty `CSI =` mode, which wezterm drops;
  - in `links` and `prompts`, erasing, editing and scrolling only after CR
    or CUB, so no wrap is pending (see `erase` and `edit`), and no SD
    (which moves soft-wrap flags its own way in each engine);
  - in `prompts`, no IL, DL, or ED 2 but from the home position: Ghostty
    handles marks its own way there (the family's reason).

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
the engines that decide it (`by`, which `--list` prints): its named cases
(each pins one point of the verdict), then random cases from it with plain
text, where those engines vote as the panel does in `run`. For the VT
families that is xterm alone, so any field xterm can tell must equal
xterm's. For a feature xterm does not implement (OSC 133), it is the
engines that do what the feature's spec says. `cases` fails a named case
in a decided family where its deciding engines outvote fux-vt. The reason
says which reference and choice, where the engines stand, and a `replay
--engines all` that shows it.

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

Beside them, real traffic: `corpus` is every recording in turn, over and
over to the same size, at 40×120, the size they were recorded at. `bench
corpus` adds each recording alone (`corpus:vim` and so on); a run without
names leaves those out, to stay a few minutes.

An engine linked in is timed on parsing and applying alone. An engine in its
own process also pays for the pipe to it, so its figure (marked `*`) is end
to end.

## The corpus

`corpus/` holds what real programs wrote to a terminal, byte for byte, while
keys were typed into them: one recording a scenario, `NAME.bin` (the bytes)
and `NAME.json` (the program and its version, the command, the size, the
environment, the replies fux-vt gave, and each step's keys and where its
output ends). The keys typed are in `corpus/keys/NAME.keys`.

### Recording

`record` runs a program on a PTY of 40×120 with `TERM=xterm-256color`, as
fux runs a pane. A fux-vt parser, set up as fux sets up a pane's (events,
DECRQM answers, in-band resize, colour-scheme reports, hyperlinks and
prompt marks: `src/pane.rs`), reads the output beside the PTY, and its
replies are written back as fux writes them, so a program that asks (DA1,
DECRQM, a cursor report) gets fux's answer, and one that asks what fux does
not answer (DA2) gets nothing, as in fux. fux answers the colour queries
(OSC 10 and 11, `CSI ? 996 n`) with its client terminal's colours
(`src/outer.rs`); the recorder answers as a fixed terminal would, white
on black, dark (`src/record.rs`), not as whoever records. The keys are
typed as fux gives them to a pane: read by fux's decoder as from a legacy
terminal, and written by its encoder in the key mode the program asked
for (the kitty keyboard protocol, modifyOtherKeys, cursor keys), so helix
gets Escape as `CSI 27 u` and vim Ctrl-D as `CSI 27 ; 5 ; 100 ~`. Step 0 is the
program starting; each line of keys is a step, typed at once, and the step
ends when the program has been quiet for a while (400 ms, or as the keys
file says). After the last step the program has two seconds to exit, then
gets SIGHUP and SIGKILL; what it writes meanwhile belongs to the last step.

`corpus/record.sh` records every scenario again, or those named. Each runs
in a directory of its own, `/tmp/fux-corpus`: a HOME with a minimal rc file
for each program (no prompt shows a user or host name), and a work
directory of copies of files from this repository and generated text (a man
page written for the purpose, `corpus/fux-corpus.1`; a small cargo project
with mistakes, for helix's diagnostics). The environment is only what the
manifest lists, and `TERM`. `git` runs on this repository, with a log
format that leaves out authors. GNU ls puts the host name in its `file://`
URIs, so it is replaced by `localhost` (`--scrub`, recorded in the
manifest). Before a recording is committed, `record.sh` says how to check
that it holds nothing private.

| Recording | Program | Steps | Bytes | What |
| --- | --- | ---: | ---: | --- |
| `vim` | VIM 9.1 | 24 | 25213 | a Rust file, syntax on: move, scroll, search, `*`, visual mode, `:split`, `:set spell`, quit |
| `helix` | helix 25.07.1 | 16 | 63811 | a cargo project with errors; rust-analyzer's diagnostics after a save; move, search, select, split |
| `less` | less 668 | 12 | 24348 | this README: lines, pages, search, the end, the start |
| `fzf` | fzf 0.65.2 | 9 | 24510 | full screen, filtering files as a query is typed, moving, accepting |
| `fzf-height` | fzf 0.65.2 | 7 | 12983 | `--height=40% --layout=reverse --border`, below the prompt |
| `gls` | GNU ls 9.12 | 1 | 1821 | `--color=always --hyperlink=always -F` on two directories |
| `man` | man (macOS, mandoc) | 8 | 8431 | `corpus/fux-corpus.1`, paged by less: lines, a page, search |
| `delta-log` | delta 0.19.2, git 2.51 | 8 | 34377 | `git log -p -n 3` through delta, paged by less |
| `delta-diff` | delta 0.19.2, git 2.51 | 5 | 45384 | `git diff` through delta `--side-by-side`, paged by less |
| `zsh` | zsh 5.9 | 17 | 1549 | ZLE: type, move, fix a word, run, Tab completion, history, Ctrl-R |
| `bash` | bash 5.3 | 17 | 877 | readline: the same keys |
| `tmux` | tmux 3.7c | 21 | 15838 | a server of its own (`-L`, `-f /dev/null`): splits, zoom, copy mode, a second window |
| `claude` | Claude Code 2.1.288 | 3 | 10616 | a first start (no settings: the theme is asked for), Ctrl-C twice |
| `claude-main` | Claude Code 2.1.288 | 3 | 2848 | a later start (onboarding done, the directory trusted): the main screen, Ctrl-C twice |
| `claude-ghostty` | Claude Code 2.1.288 | 3 | 3049 | as `claude-main`, with `TERM_PROGRAM=ghostty`, as fux passes on from Ghostty |

`claude-ghostty` is there because fux passes its own environment on to its
panes: a program in a pane of fux started from Ghostty sees
`TERM_PROGRAM=ghostty`, and Claude Code goes by it. With it, Claude Code
sends synchronized output (2026), OSC 8 and the kitty keyboard protocol;
without it, none of them. helix sends the same either way (tried), so it
has one recording. Claude Code's screens hold no account: it starts not
logged in.

Not installed here, so not recorded: neovim, htop, btop, lazygit, fish.

### Replaying

`corpus` replays each recording through fux-vt and the engines (default:
xterm and the panel), as a case of the recording's size with a step for
each step recorded, and compares them after every step as `run` does. fux-vt
is set up as fux sets up a pane (no reflow, the kitty keyboard protocol,
no identity), as the recordings were made, with fux's 10000 rows of history.

- **xterm decides what it can tell.** A field xterm tells that differs
  from fux-vt fails the step.
- **The panel decides the rest**: the fields xterm cannot tell (underline
  colour, pending wrap, the kitty flags), and every field once xterm
  abstains (from SGR 58 on). fux-vt fails there where the panel outvotes
  it, as in `run`.
- A field the panel outvotes fux-vt on where xterm agrees with fux-vt is
  shown in the marks (`-alacritty`), not failed. In `delta-diff`, delta
  draws its wrap marker in the last column and then sends EL 0 with the
  wrap pending; xterm, Ghostty, libvterm and fux-vt erase the marker,
  alacritty, avt and wezterm keep it.

Each recording has a status, as a family has (`STATUSES` in
`src/corpus.rs`): expected to agree, or differing for a recorded reason.
`corpus` exits 1 if one expected to agree fails, or one has no status. It
takes about five seconds. Today twelve agree; the three of Claude Code
differ, all at the end, where it sets an empty title and xterm shows its
default one, `xterm`.

### The inventory

`inventory` lists every sequence the recordings send, normalized (numbers
that only place the cursor or pick a colour are `n`; each mode and each SGR
attribute a row of its own), with how often, which programs sent it, and
what fux-vt does with it. A fux-vt parser set up as fux's reads each
sequence in turn: what it reports through `Sink::unhandled`, or answers, it
is seen doing. What it consumes without a word is named from its source:
private modes `Screen::mode` does not keep, SGR parameters `Screen::sgr`
passes over or reads in part, the OSC numbers `Parser::dispatch_osc` drops,
and every DCS, APC, PM and SOS string. Those lists are in
`src/inventory.rs`, and must follow fux-vt. `corpus/INVENTORY.md` is its
output, made again with the recordings.

## Transparency

A program inside fux should look exactly as it does with no fux in
between. `transparency` checks that on every recording.

```sh
fux-vt/compare/run.sh transparency                    # every recording, read by Ghostty; exit 1 on a difference
fux-vt/compare/run.sh transparency --engines in-process vim   # read by each engine in this process
fux-vt/compare/run.sh transparency --chunk 64         # compared every 64 bytes too
fux-vt/compare/run.sh transparency --size 3x10 '\e]133;A\x07$ '   # output given, as `replay` takes it
fux-vt/compare/run.sh transparency --multiplexers     # tmux's and zellij's scores beside fux's
fux-vt/compare/run.sh transparency --json FILE        # the results as JSON too
```

The output goes two ways, to two terminals of one kind (Ghostty unless
`--engines` names others; they must run in this process):

1. **Directly**, at the recording's size.
2. **Through fux**, used as a library as its server runs it
   (`src/transparency.rs`, `Through`). A `Session` with one pane and one
   client a row taller than the recording, so that the pane, above the
   bar, is the recording's size (the placement is checked). The output is
   read into the pane as the server reads it (`Session::output`, 16 KiB at
   most a read), and the pane's replies are taken as a program reads them.
   The client's terminal gets what `fux attach` writes to it first
   (`client::ENTER`: the alternate screen, autowrap off), what the server
   sends it outside paints (its queries), and every paint, made as
   `Server::paint` makes it: composed into the spare grid
   (`render::compose_into`); nothing sent if it is the screen the client
   shows; else `render::paint_into` from the shown grid, or a full paint
   the first time, and the grids swap.

Then the pane's rectangle of the client's terminal is compared with the
direct terminal, field by field (`snapshot::differences`), on what the
engine can tell (its `Can`) and a screen shows (`SHOWN`): each cell's text,
width, style and link, the rows where a prompt starts, cursor visibility,
and where the cursor is when either side shows it. Left out, as nothing on
the screen shows them and a multiplexer keeps them for the pane: soft-wrap
flags and a pending wrap (fux places every run it paints, with autowrap
off), the modes (they say how keys and the mouse are read, which fux does
for the pane), the title (fux shows it in its bar), reports and history.

**Where it compares:** after each step, and inside one before each BSU and
after each ESU of synchronized output (2026); `--chunk N` adds a point every
N bytes. The client is painted at every point. While the pane holds a frame,
fux shows the screen from before it, by design, so the point is painted but
not compared; the frame is compared at its end. A frame still held when
the recording ends is released, as the server releases one when its
timeout passes, and painted.

**What the mirror leaves out:** paints are made at every point, not spaced
16 ms apart as the server spaces them; a client that stops reading (the
server's output cap) does not happen; the client's terminal's answers to
the server's queries are not sent back (they say how to read keys and
which colours to answer a pane's colour queries with, and the recording's
output is fixed). The recordings have no resizes.

**A difference is fux's, or where fux-vt and the engine read the program's
bytes apart.** Both sides are read by one engine, so it is never the
engines splitting on fux's paint. Where fux-vt reads the program's own
output otherwise than the engine, fux paints what fux-vt has, and the two
sides differ; the corpus shows the same split (`corpus`'s marks). Such a
difference is recorded (`KNOWN` in `src/transparency.rs`) with its reason
and the differences it covers, and any other difference in the same
recording still fails:

| Recording | Engines | Why |
| --- | --- | --- |
| `tmux` | ghostty, alacritty | tmux pads its status line with ECH in black on green; fux-vt's erased cells keep the foreground, as xterm's do, Ghostty's and alacritty's only the background. A blank's foreground is not drawn |
| `delta-diff` | alacritty, avt, wezterm | the wrap marker delta erases with EL 0 while a wrap is pending: these keep it, fux-vt (as xterm, Ghostty and libvterm) erases it |

Today 14 recordings are identical beside Ghostty and the tmux recording
differs as recorded: 171 points compared, in half a second (0.3 s of
CPU; 1.5 s through `run.sh`, with its build check). With
`--chunk 13` the recordings but `tmux` stay identical at 19837 points (a
minute). `--chunk` with libvterm finds only libvterm's own handling of
UTF-8 split between writes. `cargo test` keeps every recording
transparent beside Ghostty (`every_recording_is_transparent`).

**Found:** fux does not pass prompt marks (OSC 133) on to its client's
terminal, so a prompt's row is marked directly and not through fux:
`transparency --size 3x10 '\e]133;A\x07$ '`. No recording sends them yet.

### Other multiplexers

`--multiplexers` replays each recording through tmux and zellij (each if
installed), and scores them: the share of recordings, and of steps, whose
pane looks the same as the direct screen at the end of each step, beside
fux's own share at the same points. Their differences are scored, not
failed. Each runs as a real server of its own, with nothing of the user's:

- **tmux**: `tmux -u -S SOCKET -f /dev/null new-session`, its socket in the
  harness's directory (`-S`, not `-L`, as for the tmux engine), with its
  status line under the pane, so its client is a row taller.
- **zellij**: its own configuration (no pane frames, tips, release notes,
  session saving or mouse), a layout of one pane and no bars, its own
  `HOME`, configuration and data directories, and `ZELLIJ_SOCKET_DIR`.

The client runs on a PTY with `TERM=xterm-256color` and
`COLORTERM=truecolor`, as a modern terminal sets them, and what it writes
goes to the reference engine. The pane runs the pane program the
process engines use (`src/engines/pane.rs`): each step's output goes into
it, synced by DA1, and the screen is read once the client has been quiet
for 200 ms. The terminal's answers to the client's queries are not sent
back.

Today, beside Ghostty (about 40 s for tmux and 50 s for zellij, 90 s in all;
the same scores in two runs):

| Multiplexer | Recordings identical | Steps identical |
| --- | ---: | ---: |
| fux 0.17.0 | 14 of 15 (93.3%) | 134 of 154 (87.0%) |
| tmux 3.7c | 9 of 15 (60.0%) | 127 of 154 (82.4%) |
| zellij 0.44.3 | 12 of 15 (80.0%) | 127 of 154 (82.4%) |

tmux leaves out hyperlinks (its `hyperlinks` feature is off for `xterm*`),
keeps delta's erased wrap marker, and draws blanks with the default
foreground; zellij keeps the wrap marker, makes URLs it finds into links,
and keeps ECH's foreground as fux-vt does.

**JSON** (`--json FILE`), for the scoreboard: `transparency` writes
`{"check": "transparency", "multiplexer": "fux", "version", "chunk",
"seconds", "ok", "results": [...]}`, a result for each engine and
recording: `engine`, `recording`, `steps`, `bytes`, `points`, `held`,
`points_differing_as_recorded`, `points_differing`, `step_ends`,
`step_ends_identical`, `identical`, `recorded` (the reason, or null),
`paints`, `painted_bytes` and `first_difference` (`step`, `offset`, `at`,
and each difference's `key`, `directly` and `through`), or null.
`--multiplexers` writes `{"check": "transparency-multiplexers",
"results": [...]}`, a result for each engine and multiplexer (fux first):
`engine`, `multiplexer`, `version`, `recordings`, `recordings_identical`,
`steps`, `steps_identical`, `seconds`, and for tmux and zellij `each`
recording's `steps`, `steps_identical`, `identical` and
`first_difference`; or `skipped` and why.
## esctest

`esctest` runs esctest2 (`references/xterm/esctest2`, which
`references/fetch.sh` clones), xterm's conformance suite by George Nachman
and Thomas E. Dickey: 567 tests in 76 areas (a test class, one file of
`esctest/tests/`), each writing to its terminal and reading the terminal's
reports back: the cells by DECRQCRA rectangle checksums, the cursor by DSR,
modes by DECRQM, settings by DECRQSS.

```sh
fux-vt/compare/run.sh esctest                    # every test against fux-vt; exit 1 on a mismatch with the list
fux-vt/compare/run.sh esctest DECSTBM            # the tests whose name contains it (a Python regex)
fux-vt/compare/run.sh esctest --show --replays CUP   # each failure's message, and its bytes as a replay
fux-vt/compare/run.sh esctest --in-fux --xterm   # also in a real fux pane, and in a real xterm
fux-vt/compare/run.sh esctest --json FILE --logs DIR   # every result, and esctest's own logs
```

- **Directly** (`src/esctest.rs`): each area is one esctest process (Python
  3) on a PTY of 25×80, the size esctest resizes to before every test,
  which fux-vt does not do, as it refuses window operations. Its terminal is
  a fux-vt parser set up as fux's panes are (`fux::pane::OPTIONS`), with
  two reports panes leave off: rectangle checksums (`Options::
  rectangle_checksums`, added for esctest: no program in the corpus asks
  for it, and with it a program can read its screen back, which is why xterm
  refuses it by default) and `extended_replies` (DECXCPR). It reads what
  esctest writes, and its replies are written back. Every area starts on a
  fresh terminal; within one, tests run in esctest's order, as they would
  in a terminal. Each read esctest makes waits `--timeout` (esctest's own,
  1 second) for its reply, so a report fux-vt does not give fails that
  test alone; an area running past `--limit` (120 s) is stopped, and its
  unfinished tests fail. Areas run as many at a time as there are CPUs.
- **Options.** `--expected-terminal=xterm`: esctest bends its
  expectations to the terminal it is told it runs in, xterm or iTerm2, and
  fux-vt follows xterm (blanks are spaces, checksums DEC's, xterm's own
  known bugs expected to fail). `--max-vt-level=4`, a VT420: xterm's
  default (`decTerminalID`) and the level esctest's README runs a vanilla
  xterm at; it is the level with DECRQCRA, without which no cell is read.
  The 17 VT520 tests are skipped, as are the one esctest does not try in
  xterm. No `--options`: fux-vt is UTF-8 (so not `disableWideChars`) and does
  no window operations (so not `xtermWinopsEnabled`), like a default xterm.
- **The list**, `esctest-expected.txt`: every failing test, with its
  reason, one of `departure`, `spec`, `not-implemented`, `xterm-too` or
  `bug` (the file's header says what each means). The run fails on a
  failure that is not listed, and on a listed test that passes, so the list
  never goes stale. A test that passes where esctest expects xterm to fail
  ("Should have failed") passes.
- **In fux** (`--in-fux`, for `deep`): fux is built (`cargo build --release
  --bin fux`) and each area runs in the only pane of a fux server of its
  own (a directory and socket under `/tmp/fux-esctest-*`, as fux's tests
  start one; never the user's), whose shell is esctest. A client is
  attached on a PTY of 26×80, so the pane is 25×80 beside the bar; its
  terminal is a fux-vt parser answering what fux asks of a terminal (DA1,
  DECRQM, the kitty flags, the colours, white on black). fux's panes do not
  answer DECRQCRA, so the tests that read cells cannot run there: they are
  counted as skipped. The rest are set beside the direct run's: a test that
  passes in one and fails in the other is printed, and points at fux (its
  pane replies, its encoding). A line `[fux]` in the list is a test that
  fails only in a pane, `[direct]` one that fails only directly.
- **In xterm** (`--xterm`): each area also runs in a real xterm, under the
  harness's Xvfb, the reference. It is set up as esctest's README says (80
  by 25, a VT420), UTF-8 as fux is, with DECRQCRA allowed
  (`disallowedWindowOps` without `GetChecksum`) and counting a cell nothing
  was written to as a space (`checksumExtension: 8`), as esctest expects of
  a DEC terminal: xterm's own default counts such a cell as nothing, and
  then fails every test that reads one. What passes there and fails against
  fux-vt is what fux-vt lacks; `--json` lists them.

At this commit (an Apple M2 Max, 12 CPUs, loaded by other work):

| | passed | failed | skipped | time |
| --- | ---: | ---: | ---: | ---: |
| directly | 263 (47.9%) | 286 | 18 | 31 s |
| in fux | 125 (39.7% of 315 run) | 190 | 252 | 31 s |
| in xterm 411 | 427 (77.8%) | 122 | 18 | 38 s |

The 286 failures by reason: 223 `not-implemented` (left and right margins
77, the colour palette 47, DECRQM of modes fux-vt does not keep 24,
protected cells and selective erase 17, rectangle operations 13 and more),
32 `departure` (window operations), 26 `xterm-too`, 4 `spec` (fux says it
is a VT220 and fux), 1 `bug` (a cursor report in origin mode). In fux, the
only difference is DECXCPR, which panes leave off.

`--json FILE` writes, for the scoreboard:

```json
{
  "suite": "esctest2", "expected_terminal": "xterm", "max_vt_level": "4",
  "timeout": 1.0, "filter": null, "tests": 567,
  "direct": {
    "passed": 263, "passed_beyond_xterm": 2, "failed": 286, "skipped": 18,
    "pass_rate": 47.9, "seconds": 30.6,
    "areas": {"CUPTests": {"passed": 5, "failed": 1, "skipped": 0, "pass_rate": 83.3, "...": 0}},
    "tests": {"CUPTests.test_CUP_RespectsOriginMode": {"outcome": "fail", "message": "...", "listed": "not-implemented: ..."}}
  },
  "in_fux": {"...": "as direct"},
  "xterm": {"...": "as direct"},
  "differences_in_fux": ["NAME: directly passes, in fux fails (...)"],
  "differences_xterm": ["..."],
  "mismatches": []
}
```

`pass_rate` is passed over passed and failed, in percent; `in_fux` and
`xterm` are there with `--in-fux` and `--xterm`; `mismatches` are what
failed the run.

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
| `src/transparency.rs` | `transparency`: each recording directly and through fux, tmux and zellij |
| `src/bench.rs` | the workloads and the speed table |
| `src/escape.rs` | bytes as replayable text, and back |
| `src/rng.rs` | splitmix64, as in `diff/` |
| `src/record.rs` | `record`: a program on a PTY, its output recorded, fux-vt answering its queries |
| `src/corpus.rs` | the recordings: loading, replaying beside the engines, their statuses |
| `src/inventory.rs` | what the recordings send, and what fux-vt does with it |
| `corpus/` | the recordings, their keys, `record.sh` that makes them, and the man page one shows |
