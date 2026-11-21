# Handoff: continuing work on fux

Written 2026-11-21, at `fux` 0.40.0 / `fux-vt` 0.18.0 / `fuxix` 0.11.0 (tag `v0.40.0`, all three on
crates.io). Read this first, then `README.md` (the contract: commands, the pane folder's files, keys,
bounds), `DESIGN.md` (one page: the model and the parts) and `DECISIONS.md` (D1 to D115, append-only:
every choice with why and how it was decided).

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
| `master` | this fux (it was the branch `next`; an orphan branch, no history shared with fux 1) |
| `old-main` | fux 1, fux2 and the first fux3: the earlier `master`, kept as it was |
| `next` | the same commit as `master` at the switch; can be deleted once nothing points at it |
| tag `v0.40.0` | the commit published to crates.io as fux 0.40.0, fux-vt 0.18.0, fuxix 0.11.0 |

Old tags (`v0.38.0` and earlier) and the crates.io versions up to fux 0.38.0, fux-vt 0.17.0 and
fuxix 0.10.0 are fux 1's. koh depends on those old versions (fux-vt 0.3, fuxix 0.1) and is unaffected.

## The code

Three crates, no dependencies beyond them (CI checks that `Cargo.lock` names exactly these three).
About 6,460 production lines (`./sizes.sh`).

| Crate | What | Notes |
|---|---|---|
| `fuxix` | the system calls std does not give: pty, poll, FIFO, new session, window size, raw mode | every `unsafe` in the project is here |
| `fux-vt` | the terminal emulator: bytes in, a grid of cells out; widths and grapheme clusters (UAX #29, `tools/unicode.py` makes the tables); sixel scanning and cropping | `#![forbid(unsafe_code)]`; the `Term` has no public fields |
| `fux` | `src/holder.rs` (one process per pane, writes the files), `src/view.rs` (the viewer), `src/layout.rs`, `src/keys.rs` (key names, the embedded default scripts), `src/main.rs` (the command line), `src/help-agents.txt` (`fux help agents`), `keys/` (the default key scripts) | `#![forbid(unsafe_code)]` |

Rules the code follows (each is enforced or tested, not only written down):
- Non-test code never panics: `unwrap`, `expect`, indexing, `panic!` and friends are denied by clippy.
- One source of truth for every fact, in the code and in the files; no field mirrors another.
- Everything is bounded, and what is refused or lost is said in the pane's `log`.
- A plain blocking open of a file (`File::open`, `fs::read`) is refused by `clippy.toml`: a FIFO
  put there by anyone would hang the caller. Use `sys::open` and friends.
- Tests state properties; none freezes a screen.

## The pane folder (summary; the README's table is the contract)

```
in, ctl                         FIFOs: typed bytes; commands (kill, resize, notify)
status, state, log              the holder and program: pids and command; running/exited/...; what went wrong
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

A name is a thing and never a role; a fact counts in the numbering of the thing it is of. A script
that reads several files keeps the read only when the same even `generation` brackets it (the recipe
is on `fux help agents`). A script reading one file needs nothing: every file is replaced in one rename.

## Standards

Controls follow xterm (`TERM=xterm-256color`); text segmentation follows UAX #29; widths follow modern
terminals; each protocol follows its own spec (kitty keyboard, OSC 8, OSC 133, mode 2026, ...). Every
departure is a row in the README. `tools/referee` runs the same bytes through this emulator, the old
fux-vt, tmux and xterm and counts disagreements; CI fails on an unlisted one.

## Names are temporary until fux 1 is retired

So that the two never share a session: the environment variables are `FUXN_*` (to become `FUX_*`),
the session is `$XDG_RUNTIME_DIR/fux-next`, the keys are in `~/.config/fux-next/keys`, a pane's program
sees `TERM_PROGRAM=fux-next`, and XTVERSION says `fux-next`. When the owner retires fux 1, these become
the plain names in one change (DECISIONS D41 lists them: three lines at the top of `src/main.rs` and one `sed` over `keys/`, the README and the tests), followed by a release. `cargo install fux`
already installs this fux and replaces fux 1's binary.

## Build, test, try

```sh
cargo build --release
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked          # about 130 tests; a few drive real panes and take ~20 s
tools/help-agents.sh                     # runs every example on the agents' page as one sh session
./example.sh                             # the README's example
python3 tools/perf.py target/release/fux # speed beside tmux (needs tmux): flood, key echo, CPU
```

Try it without touching a fux 1 session: run it by path, with a session folder of your own
(`FUXN_DIR=… FUXN_KEYS=… ./target/release/fux view`). Never run a bare `fux` on a machine where fux 1
is installed and in use: it is fux 1.

CI (Linux, macOS, and the referee) runs on pushes and pull requests to `master` and `next`; it takes
about two minutes.

## How changes have been made

- One PR per task; the PR body has a before/after for anything a user or script sees, the line counts
  per crate, and what was not verified.
- A bug gets a test that fails on the base first; the PR body shows it failing.
- Design questions that have several defensible answers go to a panel of independent reviewers
  who vote on a fixed ballot; the result and the strongest dissent go into `DECISIONS.md`.
- Before review, blind testers use the built binary with only the README and `fux help agents`;
  their scripts are rerun on the final head.
- Review is capped at two rounds; round two checks only round one's must-fixes. A must-fix needs a
  script or a traced sequence. A finding that asks for a safeguard against the user's own action is
  answered with a README line, not code.
- Cleanup work aims at a negative line count; nothing on the keep-list (every feature in the README)
  may be lost, and nothing may get slower (`tools/perf.py`, before and after, built in one place).

## State at handoff

- CI green on `master`. 127 tests pass (`cargo test --workspace`).
- Speed (one run, one laptop, 2026-11-21): a flood of `seq 1 2000000` 1.49 s (tmux 0.99 s); key to
  echo 2.9 ms typed slowly, 4.1 ms fast (tmux about 0.1 ms; the gap is the design, files between the
  holder and the viewer, and far below what a person notices); viewer CPU while busy 10%, idle 0.1%.
- Tested by hand and by blind testers on Linux only; macOS only through CI. Not tried in a real desktop
  terminal by a tester; htop not tried (it was not installed).

## Decided by the owner (do not reopen without new evidence from use)

- No copy mode: `C-b c` opens the pane's history and screen in `$EDITOR`. No zoom. No observing viewer.
  Mouse is passed through to programs; no mouse resizing.
- No command in the binary that a script recipe can do; recipes are plain sh (perl alternatives are
  documented, not required).
- Plain and styled files both stay; no header lines in files; one file per fact.
- When a pane shrinks, text is kept: rows go to history and the cursor may move (D54).
- Lowest common denominator of Linux and macOS; no platform workarounds; no Windows.
- The pane folder's format: D114 (folder per screen, `shown`, `generation`).
- Kitty graphics is wanted eventually, beside sixel (overruling a panel that said never).

## Open work, roughly in order

1. **Daily use.** The owner is to use this fux daily beside fux 1; what annoys or surprises in use
   decides the next changes better than another audit (the last line-by-line audits found almost
   nothing left to cut).
2. **Small known items:** `C-b c`'s menu caption still says "history in the editor" (it opens history
   and screen); `keys/.edit` passes no line number to an `$EDITOR` that is a wrapper script around a
   known editor (it falls back to line 1; `FUXN_EDIT` is the workaround); a pane kept by a holder from
   an older build writes the old file layout and nothing says so; in the pane picker the first Enter
   can be lost; `.restore` restarts the pane of a detached viewer; two role-named items in the code
   (`term.screen()`, `Holder.facts`); the word `shown` is both the link and a word of `cursor`.
3. **Kitty graphics**, in three steps: (a) output only: pictures fux has are drawn in the outer
   terminal with the kitty protocol, which is what makes pictures work in terminals without sixel
   (Ghostty), about 150 to 250 lines; (b) input of raw pixels with placement and deletion by id,
   stored as pane files beside `pic-N.sixel`, about 300; (c) PNG and compressed input (needs an
   inflate of its own: no dependencies), about 300 more. Shared memory, file transfer and animation
   are out.
4. **Retire fux 1:** take the plain names (above), delete nothing of `old-main` (it is the record),
   release.
5. **koh:** whether koh (the owner's remote shell) can carry pane folders between machines instead of
   needing its own emulator. A spike once the folder format has stopped changing.
