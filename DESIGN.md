# Design

fux is a view over the filesystem and pane daemons. Everything else follows.

## The model

- **What exists.** A session directory. In it, workspaces: folders. In a
  workspace, panes: folders, each kept by one holder process for as long as
  its program runs, and a `layout` file. Viewers: processes that read all of
  this. Key scripts: files.
- **One source of truth for each fact.** What a terminal shows: the holder's
  emulator, published as a folder for each of its two screens (`main/`
  always, `alt/` while a program has the alternate screen), the same names
  in each. Which of them is drawn: the link `shown` (that `alt/` is there
says the same in the same round, of the program). Whether a pane lives:
  the kernel's lock on its directory, held by its holder. Which run of a
  program a folder is of: its `run`, written once. Which panes exist:
  the folders. Where they are drawn and which has the keys: the `layout`
  file. What a key does: the file of its name. Whether the pane's files
  agree: that `generation` is even. A name is a thing and never a role:
  `main/screen` is the main screen whichever is drawn, and a fact counts in
  the numbering of the thing it is of (a screen's links in that screen's
  lines, the marks in the pane's text). In the code as in the
  files: no field mirrors another, and what is kept of a file is kept under
  that file's stamp and read again when the stamp changes.
- **What is derived.** The viewer's picture, from the screens and the layout.
  The layout's panes, from the folders (a folder it does not place gets a
  tab; a name with no folder is dropped). The menu, from the keys directory.
- **What changes it, and who may.** A program writes to its terminal. Anyone
  writes to `in` and `ctl`, makes, renames and removes folders, edits
  `layout`. Nothing is asked of a server, because there is none.
- **What makes a change correct.** A file is replaced in one rename, so it
  is always whole; `shown` too. `generation` is odd from the first change of
  a round of writing to its last, so files read between two looks at the
  same even number are of one round. History is appended before the screens
  that no longer show those lines, and holds whole lines: one that never
  ends is ended there, in the text's numbering too. A layout edit is read, change, rename, under a
  lock on the workspace folder. A pane's name is claimed by `mkdir`, and
  the pane by the lock on its folder.

## The parts

Three crates in one workspace; the two beside the program name nothing of it, and only fuxix may write `unsafe`.

- `fux-vt/` emulates the terminal (`lib.rs`, `width.rs`) and sizes and cuts sixel pictures (`sixel.rs`).
- `fuxix/` owns the Unix FFI: the pty, `poll`, opens that do not wait, a signal, raw mode that is put back when dropped.
- `src/` is fux, the program. `holder.rs` owns the pty, emulator and pane files. `layout.rs` places the panes.
- `view.rs` draws them and `keys.rs` names keys for the scripts in `keys/`.
- `main.rs` dispatches commands. Run `./sizes.sh` for the current line counts, crate by crate.

## How it works

**Starting.** `fux start` makes the pane's folder (`mkdir` is the claim of
a name, and a name that is there is refused: a pane is one run, so of two
starts of one name the one whose `mkdir` made the folder runs), enters it,
locks it as the directory it stands in, and
starts `fux hold` there in a new session with the
pane's lock as its standard input: the lock lives exactly as long as the
holder. The start
is done when the holder says, on a pipe, that its first screen is written (with `--out`,
that `state` says `running`).
The holder
works inside the folder with relative names, so it does not care what the
folder is called.

**Holding.** One loop waits on the pty, `in`, `ctl` and a pipe on which a
waiting thread reports the program's end. No signal is handled anywhere.
Output goes through the emulator; when the program has gone quiet for a moment, and at a
steady pace while it keeps writing (the README's "Bounds" has the numbers), a round is
written: `generation` (odd), then each file that changed (a fact in a small file of its own, a
screen's text, which is text and nothing else, in its folder), then `generation` (even). A
program that takes the alternate screen has `alt/` filled before `shown` is turned to it, and
one that leaves it has `shown` turned back before `alt/` is removed. When the program ends the holder writes `state` and its last screen
(once: a failure is a line in `log`), removes its FIFOs and leaves: `fux wait` returns after that. `kill` closes the pty's master: that
is a hangup, and the rest is the kernel's and the program's business. If the
folder is gone, the pane is gone: the holder leaves, which hangs up too.
A program started with `--out` has no terminal: its output and errors are one
file, `out`, whose bytes the holder never touches; the loop waits on `ctl` and
the pipe alone, and writes `state` as a round. There `kill` is a signal the
holder sends the program's group (a hangup, unless another is named), until it
has seen the program end; the folder gone is that hangup too, and so is the
program's end. It reaps the program only after that: the number it signals is the
group's throughout.

**Viewing.** The viewer has no channel to holders but each pane's `in` and `ctl`. It looks at files (often just after something happened, seldom at rest), reads a pane's
facts and the `screen.styled` that `shown` leads to when the pane's `generation` is another file
(it is written last), keeps what it read if it is still that file afterwards, and
parses the screen with the same emulator that wrote it. It sends keys with a write to `in` that never waits, opening it by
its path each time, so that the pane typed into is the pane of that name
now, and only if a FIFO has that name (no link there is followed, and no file written). There is no header and no version: what says that
files are of one moment is `generation`, read before and after them (in a
folder made by hand they may still disagree: the viewer takes the rows that
are there and keeps the cursor inside them). It sets the
outer terminal's cursor-key and keypad modes to the focused program's, so key
bytes pass through untouched; mouse reports it asks for in the SGR form, and
gives the program in its own, counted from the tile's corner. Pictures are files the viewer passes on after
checking that each is a file under its own name (it follows no link there) and one sixel string and that its pixels, measured by
the viewer in its own terminal's cells, are all in its tile; of any other
it writes the part inside the tile (`fux-vt/src/sixel.rs`).

**Keys.** The viewer names each key and looks for a file of that name in the
keys directory (or the open menu's directory). The layout is changed by the
scripts through `fux layout`, an edit of the file; the viewer sees the file
change like anyone else.

## What it deliberately has not

No guard against `mv`, `rm` or `kill -9`; no staging folders, markers or sweeps;
no pane ids; no recipes or restart; no kill of process trees; no embedded
language; no socket. See `DECISIONS.md` for why, and for which of the old
fux's bugs this removes.
