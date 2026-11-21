# Design

fux is a view over the filesystem and pane daemons. Everything else follows.

## The model

- **What exists.** A session directory. In it, workspaces: folders. In a
  workspace, panes: folders, each kept by one holder process for as long as
  its program runs, and a `layout` file. Viewers: processes that read all of
  this. Key scripts: files.
- **One source of truth for each fact.** What a terminal shows: the holder's
  emulator, published as `screen` and `screen.styled`. Whether a pane lives:
  the kernel's lock on its directory, held by its holder. Which panes exist:
  the folders. Where they are drawn and which has the keys: the `layout`
  file. What a key does: the file of its name.
- **What is derived.** The viewer's picture, from the screens and the layout.
  The layout's panes, from the folders (a folder it does not place gets a
  tab; a name with no folder is dropped). The menu, from the keys directory.
- **What changes it, and who may.** A program writes to its terminal. Anyone
  writes to `in` and `ctl`, makes, renames and removes folders, edits
  `layout`. Nothing is asked of a server, because there is none.
- **What makes a change correct.** A screen file is replaced in one rename,
  so it is always whole. History is appended before the screens that no
  longer show those lines. A layout edit is read, change, rename, under a
  lock on the workspace folder. A pane's name is claimed by `mkdir`.

## The parts

Three crates in one workspace; the two beside the program name nothing of it, and only fuxix may write `unsafe`.

- `fux-vt/` emulates the terminal (`lib.rs`, `width.rs`) and sizes and cuts sixel pictures (`sixel.rs`).
- `fuxix/` owns the Unix FFI: the pty, `poll`, opens that do not wait, raw mode that is put back when dropped.
- `src/` is fux, the program. `holder.rs` owns the pty, emulator and pane files. `layout.rs` places the panes.
- `view.rs` draws them and `keys.rs` names keys for the scripts in `keys/`.
- `main.rs` dispatches commands. Run `./sizes.sh` for the current line counts, crate by crate.

## How it works

**Starting.** `fux start` makes the pane's folder and locks it (both under
a lock on the workspace, so that two starts never mix), and starts
`fux hold` in a new session with the pane's lock as its standard input: the
lock lives exactly as long as the holder. The start is done when `status`
names that holder. The holder
works inside the folder with relative names, so it does not care what the
folder is called.

**Holding.** One loop waits on the pty, `in`, `ctl` and a pipe on which a
waiting thread reports the program's end. No signal is handled anywhere.
Output goes through the emulator; when the program has written nothing for a millisecond (but
not within 8 ms of the last time), and every 25 ms while it keeps writing, the screens are
written: first each fact that changed, in a small file of its own, then the two screens, which
are text and nothing else. When the program ends the holder writes `state` and its last screen,
removes its FIFOs and leaves: `fux wait` returns after that. `kill` closes the pty's master: that
is a hangup, and the rest is the kernel's and the program's business. If the
folder is gone, the pane is gone: the holder leaves, which hangs up too.

**Viewing.** The viewer has no channel to holders. It looks at files (every
4 ms just after something happened, every 50 ms at rest), reads a pane's
facts and its `screen.styled` when that file changed (it is written last), and
parses it with the same emulator that wrote it. It sends keys with a write to `in` that never waits, opening it by
its path each time, so that the pane typed into is the pane of that name
now. There is no header and no version: a reader that
takes a fact and a text one write apart is wrong for one frame, and the next
look puts it right; it keeps the cursor inside the rows it read. It sets the
outer terminal's cursor-key, keypad and mouse modes to the focused program's,
so key bytes pass through untouched and mouse reports need only their
coordinates moved. Pictures are files the viewer passes on after
checking that each is one sixel string and that its pixels, measured by
the viewer in its own terminal's cells, are all in its tile; of any other
it writes the part inside the tile (`fux-vt/src/sixel.rs`).

**Keys.** The viewer names each key and looks for a file of that name in the
keys directory (or the open menu's directory). The layout is changed by the
scripts through `fux layout`, an edit of the file; the viewer sees the file
change like anyone else.

## What it deliberately has not

No guard against `mv`, `rm` or `kill -9`; no staging names, markers or sweeps;
no pane ids; no recipes or restart; no kill of process trees; no embedded
language; no socket. See `DECISIONS.md` for why, and for which of the old
fux's bugs this removes.
