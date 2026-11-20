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
  looks every 2 s), which hangs up its program. This is how panes are
  closed. A program that ignores the hangup survives it, as after `kill`.
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

- **D21. After a picture the cursor goes to the start of the line below.**
  xterm leaves it on the picture's last row, where a shell's prompt then
  writes over the picture's cells, and here text over a picture removes it.
  The price is in the README: a picture needs a free row under it.
- **D22. A screen is at most 500 by 500 cells.** One bound for the holder,
  the emulator and the viewer (which reads at most 32 MiB of a styled screen,
  more than such a screen can be). Review round 1 found three bounds that did
  not agree.
- **D23. `fux3 start` claims a name under a lock on the workspace, and knows
  its holder is up when `status` names that holder.** Two starts of one name,
  a start and the tidying after a failed one, and a restart that fails no
  longer touch each other's files. The old status is not removed first.
- **D24. Every signal is at its default for a pane's program** (D7 said
  SIGHUP only): Ctrl-C must work in a pane started by something that ignores
  SIGINT.
- **D25. Keys typed after a key with a script wait for the script, two
  seconds at most.** Found by a tester: `C-b C-b a b c` arrived as `a b c ^B`.
- **D26. The viewer's own text (titles, the last line, the menu) goes through
  one function that makes cells**: a control character is `?`, a wide
  character two cells. Nothing from a file's name, a script's words or a
  hand-made screen file reaches the terminal as a control code.
- **D27. What cannot be written is lost and logged, not kept to try again**:
  history on a full disk, a picture whose file fails. The screen goes on.
- **D28. Round 1 findings not taken as defects**: no answer to DECRQSS and
  XTGETTCAP; `clear` leaving `history`; the bell; a picture at the bottom row
  scrolling one line (D21). All are in the README's "Left out".

- **D29. The look of fux 1, by the owner's word** ("I mainly want the look of
  fux 1 … ur new fux wastes more cells than old one"). No title rows and no
  bar row; one-cell lines with tees, green by the focused pane; the bar and
  the key menu drawn over the panes only while a prefix is in use; notices in
  the corner. Taken from a capture of fux 0.37 on a private server and from
  reading what its drawing code draws; none of that code is copied. Not
  matched: the menu's group headings and the selected line (a directory has
  no groups), choosers and prompts (none here), and `[zoom]`.
- **D30. The mouse passes through, by the owner's word** ("I dont care about
  using mouse to resize panes … I just want it to pass through"). The outer
  terminal reports the mouse only while the focused program asks. A click on
  another tile focuses it (four lines, and otherwise it would go nowhere).
- **D31. No copy mode; `C-b c` is the editor script, by the owner's word.**
  It opens in a new tab, not in a split as fux does: a new tab's program
  starts at its final size, a split's is resized in its first instant, and a
  full-screen program that is resized while it starts can draw for the old
  size (seen with vim once in six tries; any terminal resized at that moment
  does the same). The holder now draws what a program wrote before a resize
  at the old size, which removes the part of that race that was ours.
- **D32. fux's default keys** where this viewer has the action; `.about` and
  `.repeat` in a menu's directory give its name and fux's repeating maps.
  The `less`-only history key and the tab numbers went: fux has neither.

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
