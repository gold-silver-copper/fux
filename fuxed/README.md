# fuxed

A minimal screen editor written only in [fuxth](../fuxth), run by the
[`fuxth` runner](../fuxth-run). It uses no fux. Version 0 is an experiment:
its job is to find out what fuxth lacks.

```sh
fuxth fuxed/fuxed.fux FILE
```

It shows FILE on the terminal's alternate screen, with a status row: the
name, the row and column, `modified` when there are unsaved changes, and
any news. A FILE that doesn't exist opens empty, and saving creates it.

## Keys

| Key | Command | |
|---|---|---|
| arrows, Home, End | `cursor-left` `cursor-right` `cursor-up` `cursor-down` `line-start` `line-end` | move; the view scrolls to keep the cursor shown |
| PgUp, PgDn | `page-up` `page-down` | a screen up or down |
| a printable character | `insert-char` | inserts it, ASCII or not |
| Enter | `newline` | splits the line |
| Backspace, Delete | `delete-back` `delete-forward` | delete, joining lines at their ends |
| C-s | `save` | saves |
| C-q | `quit-editor` | quits; with unsaved changes it asks once, and a second C-q in a row quits |
| M-x (Alt-x, or Esc x) | `command-line` | a line of fuxth, run on Enter; Backspace edits, Esc cancels |
| Ctrl-C | | stops the command running; the status row says `interrupted` |

## How it works

It is Emacs's command loop: read a key, look up the name the keymap gives
it, find the word of that name *at that moment* (`find`), run it
(`execute`), then redraw. Commands change the buffer, a list of line
strings in a top-level value, and the values beside it (`row`, `col`,
`top`, `dirty`, `filename`, `message`), with `to`. Drawing is separate: each
frame is built as one string and written once.

The keymap holds word names, so it binds late: redefining a command
changes the next key press, with no rebinding. Calls inside definitions
bind early, as in Forth, so a helper redefined later doesn't change a
command compiled before it. At M-x:

```text
define cursor-down cursor-down cursor-down end   # Down now moves two lines
str w first ctrl str save bind                   # C-w saves
row col                                          # what is left shows on the status row
```

`bind ( k s -- )` binds a key to a word's name, and `ctrl ( c -- k )`
gives the key Ctrl and a letter sends. A line's error shows on the status
row, and the editor goes on.

**Saving** writes a new file beside FILE, `.FILE.fuxed-save`, made
exclusively, so it never opens a file that exists. When FILE exists, the new
file is made 0600 before anything is written, and gets FILE's mode once
written and closed. Its last line ends with a line end only if FILE's did
(a new file's does). Then it is renamed over FILE. Only a FILE confirmed
missing is opened and saved as new: when its metadata can't be read for
another reason (a symlink loop, a directory it can't search), it opens
empty with the OS's error on the status row, and isn't saved over.
Anything refused stops the save before FILE is replaced, and the status
row shows the OS's error.

**Out of version 0, on purpose:** sideways scrolling (a long line is cut at
the edge), tabs and wide characters drawn at their width (a tab shows as
one space), undo, search, several buffers, an init file, mouse, colour,
`fsync`.

## Tested

`fuxed/test.sh FUXTH` runs each case in `tests/` headless: fuxed opens a
copy of `in.txt` (none for a file that doesn't exist), reads `keys` (in
printf's `%b` notation) from stdin, and must quit by itself, leaving the
saved file equal to `out.txt` byte for byte and no other file. A case's
`mode` is the file's mode before, which the save must keep; its `beside/`
holds files put next to it, which must stay as they were; its `setup` and
`check` are shell run before and after (a file size limit that makes a
save's write fail, a symlink loop). The checks look at the files, never the
screen.
