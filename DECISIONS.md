# Decisions

Made by the writer alone, as the brief asked; the owner may overrule any.
Append-only.

- **D1. One binary, `fux3`, with subcommands.** The holder is `fux3 hold`.
  Smaller than two binaries, and the viewer reuses the emulator to read
  `screen.styled`.
- **D2. `FUX3C_DIR`, `FUX3C_KEYS`, `$XDG_RUNTIME_DIR/fux3c`,
  `~/.config/fux3c`.** The merged fux3 uses `FUX3_DIR` and `…/fux3`; with
  different names the two cannot meet even when both are installed.
- **D3. No signal handlers.** The holder learns of its program's end from a
  thread that waits on it and writes to a pipe. The viewer learns its size by
  asking each time round its loop. A killed holder or viewer is simply gone.
- **D4. Liveness is a lock on the pane's folder**, taken by `fux3 start` and
  inherited by the holder as its standard input. The kernel drops it however
  the holder goes. Nothing else (a pid, `status`) is proof.
- **D5. The holder works in its folder with relative names.** `mv` within a
  filesystem therefore just works, with no code for it.
- **D6. The folder is the pane.** A holder whose folder is gone leaves (it
  looks every 2 s when idle). This is how panes are closed, and it means no
  program is left running with no file to reach it by.
- **D7. `kill` is a hangup, nothing more.** No process-tree kill, no
  subreaper, no "kill-incomplete". The program's SIGHUP is set back to its
  default before it starts, so that a hangup can end it even when whoever
  ran `fux3 start` ignored SIGHUP.
- **D8. A holder that ends removes its FIFOs.** A write to an ended pane then
  makes a small file instead of waiting for ever. After `kill -9` they stay;
  the README says what that does.
- **D9. `TERM=xterm-256color`.** Keys pass from the outer terminal untouched,
  and nearly every terminal sends xterm's. Of the output side of that
  terminal description, all is implemented but the mouse, palette changes,
  margins and the visual bell (listed in the README).
- **D10. Bounds by waiting, not by dropping.** The holder reads at most
  512 KiB between two publishes (25 ms apart) and stops reading `in` at
  64 KiB waiting. Programs and writers wait, as Unix pipes make them. Only
  answers to queries and pictures over their bound are dropped, with a line
  in `log`.
- **D11. No reflow on resize.** It costs the emulator a notion of wrapped
  lines throughout; programs redraw on resize anyway.
- **D12. The layout file holds focus and the tab shown.** One truth, shared
  by viewers, and a script changes focus by editing a file. The price: two
  viewers of one workspace move together.
- **D13. The viewer writes the layout when panes come or go.** Otherwise an
  edit by a script would act on a file that does not say what is on screen.
  Found by use: the first split went to the wrong tab.
- **D14. The viewer polls files.** No inotify (Linux only) and no channel
  from holders. A stat per visible pane every 4 to 50 ms.
- **D15. Keys are named for files; the prefix is a directory.** No prefix
  setting, no key table format. Capitals are `S-x` because macOS file names
  ignore case.
- **D16. A script's output is the viewer's command line.** One channel:
  `quit`, `workspace NAME`, or a message. Errors from a script show up with
  no extra machinery.
- **D17. Closing a pane when its shell exits is the script's doing**
  (`fux3 wait`, then `rm -r`). The core keeps an ended pane's last screen.
- **D18. A new pane starts at `COLUMNS`x`LINES` if set.** The viewer sets
  them for scripts, so a new tab's program starts at the right size instead
  of redrawing after a resize. They are removed from the program's own
  environment, where they would override the real size.
- **D19. Pictures are passed through, never decoded.** A picture is a file
  and a `pic=` word on line 1. One that does not fit its tile is not drawn.
  A second protocol is a second extension.
- **D20. rustfmt at 120 columns.** Stated because the line bands are in lines.

## The old fux3's bugs: which classes are gone

From the seventy-odd reproductions in the notes of 2026-11-18.

Gone, because the machinery is gone:
- staging names, `.gone` markers and sweeps (a lookalike folder erased, half-made names);
- the session lock around kill and its timeouts;
- `mv` guards (the holder never uses its own name);
- workspace and pane ids (an edit landing on a re-made workspace);
- the `.view/ctl` layout channel (lost commands): layout edits are file edits;
- recipes and restart; zoom and template routing; grift, its questions and modal;
- recursive workspace close (it is `rm -r`, said plainly);
- the keeper, undertaker and mailbox proposed for killed holders.

Still here, because any such design has them:
- **a FIFO with no reader** makes a plain write wait for ever. Only after a
  holder is killed outright (D8); the README says so. The viewer never waits.
- **what `kill` leaves running** (D7): now stated, not fought.
- **a write to `in` just as the pane ends** can be lost. Nothing reports it.
- **history under floods**: bounded by rotation and by the read budget; a
  reader between the rename and the new file sees a short `history`.
- **atomic publish and its order**: history, then pictures, then the screens,
  then `status`.
- **a stalled disk** stalls the holder (one thread): the program then waits.
- **acting on a name that was re-used** between a look and an act: fux3 does
  it only in `fux3 start` (under the lock) and the default `x` key.
