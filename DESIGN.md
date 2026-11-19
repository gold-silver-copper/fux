# Design

fux3 is a view over the filesystem and pane daemons. Everything else follows.

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

| File | Lines | What |
|---|---|---|
| `src/sys.rs` | ~190 | the system calls std lacks, declared by hand: a pty, `poll`, `mkfifo`, a new session with a controlling terminal, window size, raw mode. The only `unsafe` |
| `src/term.rs`, `width.rs` | ~1,130 | the terminal emulator: parser, grid, colours, wide and combining characters, scroll regions, the alternate screen, queries, sixel taken whole |
| `src/holder.rs` | ~350 | one pane: the pty, the emulator, the files, the two FIFOs, one `poll` loop |
| `src/layout.rs` | ~470 | the layout file: read, settle against the folders, edit, place tiles |
| `src/view.rs`, `keys.rs` | ~670 | the viewer: draw what changed, name keys, run scripts |
| `src/main.rs` | ~200 | the command line: `dir`, `start`, `wait`, `view`, `layout` |
| `keys/` | ~170 | the default key scripts, plain `sh` |

## How it works

**Starting.** `fux3 start` makes the pane's folder (that claims the name),
locks it, and starts `fux3 hold` in a new session with the lock as its
standard input: the lock lives exactly as long as the holder. The holder
works inside the folder with relative names, so it does not care what the
folder is called.

**Holding.** One loop waits on the pty, `in`, `ctl` and a pipe on which a
waiting thread reports the program's end. No signal is handled anywhere.
Output goes through the emulator; at most every 25 ms the screens are
written. When the program ends the holder writes its last screen and
`status`, removes its FIFOs and leaves. `kill` closes the pty's master: that
is a hangup, and the rest is the kernel's and the program's business. If the
folder is gone, the pane is gone: the holder leaves, which hangs up too.

**Viewing.** The viewer has no channel to holders. It looks at files (every
4 ms just after something happened, every 50 ms at rest), reads a pane's
`screen.styled` when it changed, and parses it with the same emulator that
wrote it. It sends keys with a write to `in` that never waits. It sets the
outer terminal's cursor-key and keypad modes to the focused program's, so key
bytes pass through untouched. Pictures are files the viewer passes on after
checking that each is one sixel string.

**Keys.** The viewer names each key and looks for a file of that name in the
keys directory (or the open menu's directory). The layout is changed by the
scripts through `fux3 layout`, an edit of the file; the viewer sees the file
change like anyone else.

## What it deliberately has not

No guard against `mv`, `rm` or `kill -9`; no staging names, markers or sweeps;
no pane ids; no recipes or restart; no kill of process trees; no embedded
language; no socket. See `DECISIONS.md` for why, and for which of the old
fux3's bugs this removes.
