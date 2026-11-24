# Handoff: continuing work on fux

Written 2026-11-21, at `fux` 0.40.0 / `fux-vt` 0.18.0 / `fuxix` 0.11.0 (tag `v0.40.0`, all three on
crates.io). Read this first, then `README.md` (the contract: commands, the pane folder's files, keys,
bounds), `DESIGN.md` (one page: the model and the parts) and `DECISIONS.md` (D1 to D124, append-only:
a historical log, in which a later entry says so when it replaces an earlier one).

## What fux is

A terminal multiplexer that is a view over the filesystem and pane daemons.

- A **pane** is a directory kept by one **holder** process: it owns the program's pty and a terminal
  emulator, and publishes what the terminal shows as files, one file per fact. It takes typed bytes
  from the FIFO `in` and commands from the FIFO `ctl`.
- A **workspace** is a folder of panes, with a readable `layout` text file.
- The **viewer** is one more client: it draws a workspace's panes as tiles and tabs and sends keys
  to the focused pane.
- A **key binding** is an executable shell script in a directory tree (`keys/C-b/v` is "prefix,
  then v"; a folder is a menu).
- No server, no socket. `cat`, `ls`, `printf`, `rm` are clients. Behaviour is emergent from what the
  filesystem and processes do; there are no safeguards against a user's own `mv`, `rm` or `kill`
  (the README's "don't" list says what happens instead).

## Branches and history

| Name | What it is |
|---|---|
| `master` | this fux, since 2026-11-21 (it was the branch `next`, an orphan branch with no history shared with fux 1) |
| `old-main` | fux 1, fux2 and the first fux3: the earlier `master`, kept as it was |
| `next` | the same commit as `master` at the switch; can be deleted once nothing points at it |
| tag `v0.40.0` | the commit published to crates.io as fux 0.40.0, fux-vt 0.18.0, fuxix 0.11.0 |

Old tags (`v0.38.0` and earlier) and the crates.io versions up to fux 0.38.0, fux-vt 0.17.0 and
fuxix 0.10.0 are fux 1's. koh depends on old versions (`fux-vt = "0.3.4"`, `fuxix = "0.1.7"`) and is
unaffected. `tools/referee` builds the old emulator from a commit on `old-main`, so `old-main` must stay.

## The code

Three crates, no dependencies beyond them (CI checks that `Cargo.lock` names exactly these three).
About 6,870 production lines (`./sizes.sh`).

| Crate | What | Notes |
|---|---|---|
| `fuxix` | the system calls std does not give: pty, poll, FIFO, new session, window size, raw mode | every `unsafe` in the project is here |
| `fux-vt` | the terminal emulator: bytes in, a grid of cells out; widths and grapheme clusters (UAX #29, `tools/unicode.py` makes the tables); sixel scanning and cropping | `#![forbid(unsafe_code)]`; the `Term` has no public fields |
| `fux` | `src/holder.rs` (one process per pane, writes the files), `src/view.rs` (the viewer), `src/layout.rs`, `src/keys.rs` (key names, the embedded default scripts), `src/lib.rs` (the command line, which `src/main.rs` calls), `src/help-agents.txt` (`fux help agents`), `keys/` (the default key scripts) | `#![forbid(unsafe_code)]` |

What the library exports, for a program that carries fux (koh; D124): `fux::command(args)`, one command
line of fux's from its command's word on (`fux::main()` is that with the process's own arguments; the
carrier's own first word `hold` goes to it too, as `start` runs the program it is in with that word);
`fux::view(session, workspace, keys)`, the viewer; `fux::FORMAT`, the number of the pane folder's format
(the README says when it is raised); `TERM_PROGRAM`; and how fux writes and reads a pane's files:
`replace`, `staging`, `write_new`, `stamp` and `Stamp`, `lines`, `plain`.

Rules the code follows (the first and the fourth are enforced by clippy; the others are design rules,
kept by review and tests):
- No panics in non-test code: the constructs that panic (`unwrap`, `expect`, indexing, `panic!`,
  `todo!`, `unreachable!`, ...) are denied by clippy. Arithmetic overflow is not covered by a tool.
- One source of truth for every fact, in the code and in the files. The deliberate exceptions are
  caches of what was written, such as the holder's last published facts and its `generation`.
- Everything has a bound (the README gives them, under "A pane's files" and in its "Bounds" paragraph).
  Most losses are said: the holder's in the pane's `log` (which itself stops at 64 KiB), the viewer's in
  its corner. Bytes written to `in` after the program hung up are dropped without a word.
- `File::open`, `fs::read` and `fs::read_to_string` are refused by `clippy.toml`: a FIFO put there by
  anyone would hang the caller. Use `sys::open` and friends. (Other opens, `OpenOptions` for history and
  the viewer's own FIFO, are reviewed by hand.)
- Tests state properties; screens are compared only where the screen is the property.

## The pane folder (summary; the README's table is the contract)

```
in, ctl                         FIFOs: typed bytes; commands (kill, resize, notify)
status, state, log              the holder and program: pids and command; running/exited/...; what went wrong
command                         the program and its arguments as given, each ended by a zero byte
generation                      odd while the holder writes a round, even when done
size, cursor, modes             the terminal: size; the cursor on the screen shown; input modes
title, cwd, clipboard, notice   what the program said
shown -> main | alt             the screen being drawn
main/ screen screen.styled lines links pictures    the main screen, always
alt/  screen screen.styled lines links pictures    the alternate screen, while a program has it
history, history.styled (.1)    the text that scrolled off the main screen
marks                           OSC 133 prompt marks, in the text's numbering
pic-N.sixel                     a picture's bytes
```

`alt/` can also remain after a pane ended under a full-screen program. A name is a thing and never a
role; a fact counts in the numbering of the thing it is of.

Reading: the screens, `lines`, `links`, `pictures`, `cursor`, `size`, `modes`, `state` and the other
facts the holder publishes are each replaced in one rename, so one of them read alone is always
whole. `history` (appended, and cut back when a resize takes lines back) and picture bytes are written
in place, inside a round. `generation` brackets a round: a script that reads several files keeps the
read only when the same even number is there before and after, retrying a bounded number of times (the
recipe, `together`, is on `fux help agents`; a holder that died mid-round leaves it odd). Outside any
round: `log`, `cwd` (a program may write it itself), the FIFOs, and files scripts make in the folder.

## Standards

Controls follow xterm (`TERM=xterm-256color`); text segmentation follows UAX #29; widths follow fux's
own documented rule (modern terminals'); each protocol follows its own spec (kitty keyboard, OSC 8,
OSC 133, mode 2026, ...) except where the README says otherwise: its departures are rows in the README,
and what is left out (among them OSC 8 link ids, clipboard reads, sixel aspect ratios) is in "Left out".
`tools/referee` runs a corpus of byte streams through this emulator, the old fux-vt, tmux and xterm and
compares text and cursor; CI fails on a disagreement that is not listed.

## Names, and no variables

The default session is `$XDG_RUNTIME_DIR/fux`, else `~/.cache/fux`: a name is a name in it, wherever a
command is typed. Anything else is named by a directory's path (an argument that begins with `/` or
`.`). The keys are the directory named to `fux view WORKSPACE KEYS`, else `$XDG_CONFIG_HOME/fux/keys`,
else `~/.config/fux/keys`. A pane's program sees `TERM_PROGRAM=fux`; XTVERSION says `fux VERSION`.
fux sets, reads and removes no variable of its own (D123): what `FUX_DIR`, `FUX_WS` and `FUX_KEYS` said is an
argument, what `FUX_PANE` said is `keys/.here`'s to find, and what a key's script was told in five more
is a folder of files the viewer makes for it (`SESSION/.view.PID/N`). koh, which uses fux as a library
and wrote to the viewer's FIFO, has changes to make before it moves to this fux: D123 lists them.

## Build, test, try

```sh
cargo build --release
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -- --test-threads 4   # 170 tests; the real-binary suite takes ~20 s
tools/help-agents.sh                     # runs every example on the agents' page as one sh session
./example.sh                             # the README's example
python3 tools/perf.py target/release/fux # speed: flood, key echo, CPU (with tmux, beside it)
```

Needed: stable Rust with clippy, Python 3, sh, less; perl only for the help page's optional section; the
referee also needs xterm, Xvfb and tmux. Releases: `cargo publish` for `fuxix`, then `fux-vt`, then `fux`
(D115).

Try it without touching a running session: run it by path, with a session folder and a keys folder
of your own that do not exist yet (`./target/release/fux view /a/new/session/main /a/new/keys`). A bare `fux`
is whatever `PATH` finds first, which may be fux 1.

Upgrading a running session: a holder keeps the code it was started with, so after a rebuild the panes
it keeps still write the old files until they are closed and started again. The viewer writes the default
key scripts only when the keys folder does not exist: remove it (or name a new one to `fux view`) to
get the new defaults.

CI (Linux, macOS, and the referee) runs on pushes and pull requests to `master` and `next`; it takes
about two minutes.

## How changes have been made

- One PR per task; the PR body has a before/after for anything a user or script sees, the line counts
  per crate, and what was not verified.
- A bug gets a test that fails on the base first; the PR body shows it failing.
- A test of the real binary (`tests/panes.rs`) has one deadline, two minutes from its first session.
  It starts a command with `run`, `run_from`, `ended` or `begin`, writes to a FIFO with `send`, reads
  with `read` (which never waits) and waits with `until` or `until_or`: their waits end at that
  deadline, and a command started through them is one the tests stop when they stop. Nothing checks that a new test does so (a bare
  `spawn`, `output` or `fs::write` to a FIFO waits by itself): look for it in review. Run them all
  under `timeout` as well.
- Design questions that have several defensible answers go to a panel of independent reviewers
  who vote on a fixed ballot; the result and the strongest dissent go into `DECISIONS.md`.
- Before review, blind testers use the built binary with only the README and `fux help agents`;
  their scripts are rerun on the final head.
- Review is capped at two rounds; round two checks only round one's must-fixes and what their fixes
  touched. A must-fix needs a script or a traced sequence. A finding that asks for a safeguard against the user's own action is
  answered with a README line, not code.
- Cleanup work aims at a negative line count; nothing on the keep-list (every feature in the README)
  may be lost, and nothing may get slower (`tools/perf.py`, before and after, built in one place).

## State at handoff

- CI green on `c5645729b`, the published commit; 127 tests.
- Speed: the README's table is an earlier run. A later one (one laptop, 2026-11-21, at `3ebc9fb41`):
  a flood of `seq 1 2000000` 1.49 s (tmux 0.99 s); key to echo 2.9 ms typed slowly, 4.1 ms fast (tmux
  about 0.1 ms; the gap comes from the design, files between the holder and the viewer); viewer CPU
  while busy 10%, idle 0.1%. The per-screen change after that cost the busy repaint a little (699 to
  695 frames a second on a bench machine; the flood itself was 2.33 s before and 2.23 s after there).
- Tested by hand and by blind testers on Linux only; macOS only through CI. Not tried in a real desktop
  terminal by a tester. htop is in the referee's recorded sessions, but no tester ran it live.

## Decided by the owner (do not reopen without new evidence from use)

- No copy mode: `C-b c` opens the pane's history and screen in an editor (as an executable `.editor` in the keys folder runs it, if there is one, else
  `$EDITOR`, else `$PAGER`, else `less`). No zoom. No observing viewer.
  Mouse is passed through to programs; no mouse resizing.
- No command in the binary that a script recipe can do; recipes are plain sh (perl alternatives are
  documented, not required).
- Plain and styled files both stay; no header lines in files; one file per fact.
- When a pane shrinks, the main screen's text is kept: rows go to history and the cursor may move
  (D54). The alternate screen is cut or padded; it has no history.
- Lowest common denominator of Linux and macOS; no workarounds for a platform's kernel bugs (D46: a
  failed system call is reported, never retried). The holder keeps the pty's slave open while the
  program runs, which keeps a program's last output on macOS too. No Windows.
- The pane folder's format: D114 (folder per screen, `shown`, `generation`).
- No Kitty graphics: sixel is the one picture format (the owner, 2026-11-23, D119). It was built
  (D117, 2026-11-22) and taken out a day later: the reader of its commands, an inflate and a PNG
  decoder were over two fifths of the emulator, and the most code in fux reading what a program
  sends. A command of it is read to its end and ignored. Two smaller forms were turned down with it:
  output only (a pane's sixel pictures drawn with the Kitty protocol in a terminal without sixel),
  and a thin layer from Kitty graphics to sixel (it needs all three parsers). The kitty keyboard
  protocol stays.

## Open work, roughly in order

1. **Daily use.** The owner is to use this fux daily beside fux 1; what annoys or surprises in use
   decides the next changes better than another audit (the last line-by-line audits found almost
   nothing left to cut).
2. **Small known items:** `rounds_of_writing_that_fail_do_not_leave_their_pictures_for_good` failed once in a suite at 4 threads while D123 was built (it waited to its deadline for six failed rounds in the pane's `log`), and not again: 0 of 6 alone, 0 of 24 eight at a time, and in no later suite; not explained; `tools/help-agents.sh` failed once in CI on Linux at the page's last command (`rm -r` of the demo workspace: `agent-demo/broad/main: Directory not empty`; PR #483, 1 of 11 runs there, 0 of 160 by hand on that branch and on master): the README's "one failed `rm -r` of a busy pane", after the page typed into that pane five commands before, and not explained further; `C-b c`'s menu caption still says "history in the editor" (it opens history
   and screen); `keys/.edit` passes no line number to an `$EDITOR` it does not know by name, such as a
   wrapper script around a known editor (the file opens where that editor starts; an executable `.editor` in the keys folder is given
   the line); a pane kept by a holder from an older build writes the old file layout and nothing says so; in the pane picker the first Enter
   can be lost (seen by a blind tester; the record is in the owner's notes); `.restore` can restart a
   pane whose saved command was a viewer run by another program (`env X=1 fux view`); two role-named items in the code (`term.screen()`, `Holder.facts`); the word `shown` is both the link and a word of `cursor`.
   Known since the two reviews of PR #483, and left (a line each):
   - `keys/.restore`'s lock is its perl wrapper's: killing that one process by its number frees the lock while the run under it goes on (Ctrl-C and a hangup end both).
   - `keys/.restore` runs itself again with a second word `--alone`, which skips the lock and is in no usage line.
   - A key held before a `.rename` and another pressed after it are two open descriptors of the one folder in the viewer, until both are let go.
   - A folder with no `generation` whose tab is not shown, removed, and another made under its name that gets the same inode (ext4 may give it): the viewer shows what it read of the first when the tab is shown again.
   - A pane renamed and its old name taken by another folder, both inside one frame of a viewer, with a key down in it: the key is let go in the renamed pane at once (a release it did not have yet), not held (read from the code, not run).
   - `a_history_that_cannot_be_put_aside_begins_anew_and_the_other_keeps_its_lines` failed for a reviewer alone, 6 of 10 on the base and 9 of 10 on the branch (`55977 lines, 0 put aside`, dash as `sh`); 0 of 20 alone and 0 of 48 sixteen at a time where the branch was built. It asserts that `history` was put aside once when the screen says `done`, which depends on how the output was cut into writes; not looked into further.
   - `a_menu_shades_its_group_headings` and `a_shell_that_ends_with_127_closes_its_pane_…` fail on master and on the branch alike when 24 of them run at once (about one run in six); the second also once in 34 runs of the suite at 16 threads on the branch (`the holder ended without a word`: its shell ends at once and `.shell` removes the pane's folder, sometimes before the holder's first screen, which fails the start the test asserts).
   - `a_picture_that_does_not_fit_its_tile_…` failed once in the same 34 runs of the suite at 16 threads on the branch, at its one read of the viewer's screen (it waits for the left pane's picture, not for the right pane's word); not seen again in 144 runs of it 24 at a time, nor on master.
   Known since the review of PR #484, and left (a line each; what it found and had fixed is in D123):
   - `keys/.edit` takes a link called `workspace` in its working directory for the sign that a key's script runs it: run by hand in a directory that has such a link, it looks for `../cwd` from there, finds none, and starts the editor's pane at home.
   - A folder whose own name ends in a line end is not for the default keys (as a session, as the keys' directory, to start a viewer in): sh drops the line ends that end a `$( )`. One with a line end inside its name is served, and tested.
   - `keys/.here` given the session by a path from where it stands that begins with `-`: found now that perl is given the folders after `--` (it was `grep` that took the path for an option); `keys/.shell` hands what `.here` printed to `ls` and `rm` as it is, and every script passes a whole path.
   - A path given to `fux view` that is mistyped still makes every folder on the way to it where there is a terminal, as `fux start PATH/` does; a path that is no workspace's name leaves its empty folder, which the error names and no test asserts.
   - The viewer's folder goes on every way out but a kill, and a panic is one more: the release build aborts on a panic (none was found that can be reached), and the folder stays as after a kill.
   - Two calls of `fux::view` at once in one process (a library's user: koh makes one) would have the one folder `.view.PID`.
   - `fux dir` passes over any further argument; `fux start --help -- true` starts a pane called `--help` (only `--` is read as a separator); `fux wait` takes a directory from where it stands as it is.
   - A key script's "is still running: keys no longer wait for it" takes the place of a newer message in the viewer's corner when the script's two seconds are over (on master too); the test of the 33rd script sleeps 2.5 s for it.
   - The tests' stand-in for the viewer (`Session::key`) differs from it in two things: it sets no `COLUMNS` and `LINES` (the viewer does), and it uses the first free number for a script's folder (the viewer uses none twice). A script tested only through it is not tested for either.
   - `tools/perf.py` was changed for D123 (`XDG_RUNTIME_DIR` where `FUX_DIR` was; its viewer writes the default keys into its scratch config directory) and has not been run since.
   - koh, when it moves to this fux: D123's list, with what its own fallback for the session must become.
3. **Retire fux 1:** take the plain names (above), delete nothing of `old-main` (it is the record),
   release.
4. **koh:** whether koh (a remote shell over QUIC, a separate project) can carry pane folders between
   machines instead of needing its own emulator. A spike once the folder format has stopped changing.
