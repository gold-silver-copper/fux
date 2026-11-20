# Decisions

Made by the writer alone, as the brief asked; the owner may overrule any.
Append-only. Up to D33 the program was called fux3, its variables `FUX3C_…`
and its folders `fux3c`; those entries stand as written. D34 renamed it.

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
- **D24. The classic signals (1 to 31) are at their defaults for a pane's
  program and for its holder** (D7 said SIGHUP only), and `fux3` itself takes
  SIGCHLD's default, so that a starter which ignored it changes nothing: Ctrl-C must work in a pane started by something that ignores
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

- **D33. Review round 2 (the last): what was fixed and what was not.**
  Fixed: a starter that ignores SIGCHLD; a repeat longer than the screen; a
  malformed control sequence (now ignored whole); refusals kept until logged;
  status and last screen reported apart; the removed-directory deadline; what
  an earlier holder left kept until the new screen is written; output errors
  of the command line; the log's accounting; a failed history rotation; in
  the new parts: a wide character under an overlay, the menu's width in
  cells, a pane with no room, an edit that nests too deep, the mouse (a
  gesture stays with its press, modifiers, the old report form, reporting off
  when the program is gone, keys after a focus click), `LINES`, Alt-[, the
  editor script (never a missing line, its failure shown, `less +G`), the
  waiting clean-up (only the pane it waited for), TERM only to a running
  program. Not taken, with why: a `resize` reads up to 1 MiB outside the
  512 KiB budget (bounded, and it is what closes the resize race of D31);
  picture files can pile up while every screen write fails (it needs a disk
  that refuses the screens and takes the pictures); realtime signals stay as
  inherited; no answer to "what else is hidden" past `▼ N more` in a short
  terminal's menu; the cursor is not hidden under a notice. One round 2
  report (the emulator's) was cut off after its table of verdicts: its three
  "not closed" rows are fixed as the table words them.

- **D34. It is called fux, version 0.40.0** (the owner: "call it just fux
  not fux3, because eventually this will completely replace the old fux";
  plain names now). `FUX_DIR`, `FUX_KEYS`, `FUX_WS`, `FUX_VIEW`, `FUX_PANE`.
  The session is `$XDG_RUNTIME_DIR/fux/workspaces`, one folder further in
  than fux 1's own files, which a panel found this viewer listing as a
  workspace. A pane's program gets `TERM_PROGRAM=fux` and its version, and of
  the `FUX…` names only `FUX_PANE`, `FUX_DIR` and `FUX_KEYS`: the last two
  are this program's own and say where its session and keys are; everything
  else (a key script's, fux 1's) is taken out. Key scripts get their five.
- **D35. A command typed in a pane means "here"**: with `FUX_WS` unset, the
  workspace is the folder holding `$FUX_PANE`, and the session its parent
  unless `FUX_DIR` is set. `FUX_PANE` counts only as a whole path (fux 1's
  is `%N`).
- **D36. The layout marks the tab shown before with `-`** (panel: 6 of 8;
  the owner: "will i automatically be returned to the tab i was on
  previously? Because that is the proper UX"). One mark, like `$OLDPWD`. An
  edit that ends on another tab marks the one it left; when the shown tab
  goes the marked one is shown. `tab -` goes there (one line). The previous
  workspace is each viewer's own memory: `workspace -`, and where a viewer
  goes when the workspace it shows is removed.
- **D37. Zoom is gone** (the owner: "opinionated behavior"; panel tied 4 to 4,
  decided by less code and the owner's lean): the word, the edit, the key
  and the drawing. The README has the two scripts that do it, run before
  they were printed, and what they do not put back.
- **D38. Menu groups are a comment line**, `# group: NAME`, after the caption
  (panel: 6 of 8). Groups come in alphabetical order, not fux 1's: an order
  would need a number in every script. Keys with no group come first.
- **D39. The editor key gives a line number only to `vi`, `vim`, `nvim`,
  `nano`, `emacs` (and `+G` to `less`)**; `$EDITOR` may hold arguments. The
  owner's own `EDITOR` is a launcher that showed `+10` as a file.

- **D40. The fix check of D34 to D39 (1 Opus, 1 gpt-6.2-astra, one pass).**
  Fixed: `-` alone is no name (it is the tab mark, and "the one before" to
  the viewer); a relative `FUX_DIR` or `FUX_KEYS` is made whole; a `FUX…`
  name that is not text is taken out too; `$EDITOR` is split, not expanded;
  the menu's count of what it hides; the README's restore script (a keys
  path with a space, a failed write keeping the saved line); README and
  `fux help` where they said `main` or left out `$XDG_CONFIG_HOME`; the
  tests no longer take a surrounding pane's workspace or keys for their own.
  Not taken: the restore script choosing the first tab that holds an old
  neighbour when they have been spread over several (said in the README).

- **D41. Temporary names until fux 1 is retired, by the owner's word** ("go
  with option A for now then just to keep it simple and bare"); this
  replaces D34's names and the part of D35 about them. The program, package
  and README are `fux`; the branch is `next`; the variables are `FUXN_DIR`,
  `FUXN_KEYS`, `FUXN_WS`, `FUXN_VIEW`, `FUXN_PANE`; the session is
  `$XDG_RUNTIME_DIR/fux-next` (no subfolder) and the keys
  `…/fux-next/keys`; `TERM_PROGRAM=fux-next`, as the owner's tooling reads
  `TERM_PROGRAM=fux` as "fux 1 is here". No code knows fux 1: its variables
  pass through a pane like anyone's. The prefix, the folder and the
  `TERM_PROGRAM` word are three lines at the top of `src/main.rs`; the
  switch to the plain names is those three lines and one `sed` over `keys/`,
  the README and the tests, which spell the variables out.

- **D42. After a panel of eight read the code (2026-11-19): eleven defects
  fixed, six changes made, no split into crates.** The first line of a screen
  file is the emulator's to write and read (`Term::header`,
  `Term::from_screen`). `boundary.sh`, run by CI, compiles the emulator, the
  system calls and the key names each as a crate of its own: that is what
  keeps a later split cheap. The close key only removes the folder (`kill`
  and then `rm -rf` raced the holder's last writes and left a dead folder).
  The viewer opens a pane's `in` by its path for every write, and tells a
  lost pane by the lock on its folder. `split … beside PANE` lets the split
  keys name the pane they were pressed on. In a view too small for every
  tile, the focused pane is among those drawn. `fux wait` reads the status
  of the folder it locked, by entering it (one more system call, `fchdir`).
  A layout file may be 160 KiB, which a thousand longest names need, and an
  edit is refused if what it would write is over that.
- **D43. A default shell pane removes itself** (the orchestrator's call after
  a 4 to 4 vote: it removes mechanism and adds none). The key scripts start
  the shell inside a one-line wrapper that removes `$FUXN_PANE` when the
  shell ends, and no longer leave a process waiting to remove the folder,
  which could remove a pane started again under the same name. The editor
  key does the same. The price, in the README: a shell or editor killed
  outright leaves its folder (and the editor its text file).

- **D44. The fix check of D42 and D43 (1 Opus, 1 gpt-6.2-sol, one pass).**
  Fixed: the wrapper of a default pane is one script, `keys/.shell`, which
  outlives the TERM key, and removes the folder only if it is still its own
  (a pane moved away no longer removes whatever took its name); the close
  key moves the folder out of the workspace, removes `in`, and removes the
  folder once the holder has left, so that nothing writes into a folder
  being removed (12 of 24 busy panes were left behind by a bare `rm -rf`);
  `fux start` takes a pane that is gone with its holder as started and done
  (an editor that ends at once); a row with text is never dropped by a
  resize, it goes to history; `mousesgr` is listed on its own; the boundary
  check compiles `width.rs` alone too. This is more than "the close key only
  removes the folder" as voted: it still sends no `kill`, which was the
  point, and it is what makes the removal whole.

- **D45. The rest of the panel's table, at the owner's word** ("I want you to
  do all of them, dont wait for daily use"). One bounded string state for
  OSC, DCS, APC, SOS and PM, with the byte that began the string kept (it
  replaces two states and makes the next string feature a match arm). The
  emulator hands all it produces to the holder as one value, `Out` (answers,
  history, refusals, pictures' bytes, title, clipboard), taken with
  `Term::take`; those were loose public fields before. Titles: a `title`
  file, and the viewer sets its terminal's title from the focused pane's,
  with xterm's title stack to give the old one back. Clipboard: setting
  only; a `clipboard` file; the viewer relays a write of the focused pane
  once, and tells a fresh one by the file's having changed since it first
  looked. `pid` and `state` are on line 1 of the screens only; `status`
  holds `holder` and `command` and is written once. That removed the second
  write of `status` and its ordering against the screens (about 20 lines)
  and cost 8 in `fux wait` and one key script; it was worth doing. `replace`
  and `plain` moved from the holder to the crate's root, beside `is_name`.
- **D46. No workarounds for a platform's kernel bugs, by the owner's word**
  ("we shouldnt have workarounds for obscure macos pty stuff, the new fux
  should be ultra minimal"). A system call that fails is reported as it
  failed: `fux start` says the error and exits non-zero. Nothing retries or
  watches a system call. The code had none of that kind to take out: the
  one retry there is, in `fux start`, waits a moment for a `fux wait` that
  holds the pane's lock, which is this program's own doing, not a kernel's.

- **D47. The fix check of D45 (1 Opus, 1 gpt-6.2-astra, one pass).** Fixed:
  a pane started again in place no longer keeps the last program's title
  and clipboard; the viewer relays a clipboard only when the file of the
  pane that has the focus is another than at its last look with that same
  pane focused (so a copy made in a hidden tab, or before the focus came, is
  never sent), and it reads the file it took the stamp from; clipboard data
  that is not base64, or has no payload, changes nothing and is logged; a
  string past its bound is logged at the first byte too many; a title cut at
  256 characters and a ninth title put aside are logged; the title stack
  leaves the icon's name alone, an empty stack changes nothing, and a reset
  keeps the title; a picture pushed off by a ninth takes its bytes with it.

- **D48. A referee, and one authority for each kind of question** (the panel
  on fux's future, 2026-11-19). `tools/referee/` plays case files to the
  built program (through a pane: `in`, `ctl`, `screen`, so the whole path is
  what is compared) and to xterm under Xvfb, tmux on its own socket and the
  old emulator pinned by git revision in a package of its own (the program's
  `Cargo.lock` still lists one package). The README's table is the only list
  of departures: the referee reads it, and CI fails on a difference that is
  not a row. A row that no longer differs is reported, not failed. Its first
  run found three places where fux differed from xterm for no reason, fixed
  here: REP repeated a character across a move or a control; a C1 control
  written as UTF-8 showed U+FFFD; with wrapping off a wide character with no
  room wrote over the row's last two cells. What it found and fux keeps is in
  the table: no left and right margins, no printer, no line feed mode, SUB
  shows nothing. The Unicode and resize rows are the next two changes' work.
- **D49. Size ceilings are checked** (`sizes.sh`, in CI): the emulator at most
  2,500 lines written by hand (its two files and the tools that write its
  tables), the program at most 5,500 (non-test Rust and the key scripts).
  Generated tables are counted apart.

- **D50. The fix check of D48 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  Fixed in the referee: its player asked the terminal a question after every
  line of a case, which cut sequences written over two lines (two rows were
  wrong for it), so feeds now go as one stream and it asks only around a resize
  and at the end, taking the last answer after the terminal has gone quiet;
  fux's screen is read after the player has left and `fux wait` has returned,
  not after a pause; a witness that cannot be played fails the run and is
  never a "known" row; all three witnesses are needed unless `--witness` names
  fewer; a row says where the difference is (`text`, `cursor`) and any other
  difference on that case fails; xterm's print of its screen is told from a
  case's own printing, runs with none of the user's X resources and paths are
  quoted; Xvfb gets a number from 90 up that the referee asked for and is
  checked alive before each xterm (on the owner's machine `-displayfd` alone
  had been handing out number 0, the desktop's); work files go under
  `$TMPDIR` or `$XDG_RUNTIME_DIR`, never `/tmp` by default. Fixed in the
  emulator, as xterm does each: DECALN homes the cursor and lets the whole
  screen scroll; mode 1047 asked for again keeps the screen; REP after a
  cancelled sequence repeats nothing and after a dropped character repeats
  nothing visible; a C1 control written too long is not dropped as a C1
  control; `_` in the line-drawing set is a blank. Kept, each a row: a
  non-ASCII character inside a sequence (xterm reads it as a Latin-1 letter),
  a wide character on a one-column screen, memory lock. Not done: comparing
  answers, colours and modes (the referee sees text and cursor, and the README
  now says so); decoding tmux's tab and line-drawing cells (two rows say it).

- **D51. Characters are grapheme clusters, cut as UAX #29 says, always**
  (the panel, 2026-11-19). `src/width.rs` keeps one table (a character's
  kind for cutting, its part in a conjunct, three flags) and a `Joiner` of
  six small fields; Unicode's own GraphemeBreakTest passes whole and is kept
  in `tests/` so no test needs the network. The table is written by
  `tools/unicode.py` from five files of Unicode 17.0.0 that the tool pins by
  SHA-256 and fetches only when run by hand (1.5 MB of data files are not
  kept in the repository; the test file, 127 kB, is). A cell holds one
  cluster of at most ten characters; more are dropped and `log` says so.
  - Width: the columns of a cluster's characters added up, two at most; this
    is what terminals that do not join give for every cluster of up to two
    columns, and what Ghostty gives with mode 2027 (the seat's probe: 0 of 9
    cursors differ). Half a flag is one column, so a flag is two everywhere.
    U+FE0F widens an emoji; U+FE0E changes nothing (the rule has no word for it).
  - A cluster that grows to two columns in a row's last column goes whole to
    the next row. With wrapping off the character that would widen it is
    dropped.
  - A cluster is text with nothing between: a control, an escape sequence or
    a resize ends it. A mark or joiner with nothing to join, and a format
    character such as U+200B, takes no cell and is dropped (xterm shows none
    either); the old code hung marks on whatever was left of the cursor.
  - REP repeats the cluster's first character, as xterm's does.
  - `screen.styled` says the style again between two characters that would
    read back as one (two halves of a flag put side by side by cursor moves).
  - Mode 2027 is answered "permanently set" and cannot be switched.
  - The viewer cannot know how the user's terminal measures, so it does not
    try: it places by row and column after anything but plain ASCII, blanks
    the two cells under a joined wide character first, sends again the cells
    a character may have been drawn over, and at a tile's edge draws `?` for
    a character that could be drawn past it. The seat's probe (a Ghostty
    engine as the outer terminal, 5 strings, both modes): 0 of 10 dividers
    and 0 of 10 neighbour tiles damaged; before, 4 and 1.

- **D52. The fix check of D51 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  The cutting was right; what was wrong was around it, and one rule of D51 is
  withdrawn.
  - Withdrawn: "a control or an escape sequence ends a character". `grep
    --color` colours a letter and sends its accent after the reset, so the
    accent was lost, here and in no other terminal. Now a character goes on
    while the cursor is where its last part left it and its cell is as it was
    left: state, not a list of sequences. A style, a title, BEL, a sequence
    that comes to nothing change neither; a move or an erase does.
    `screen.styled` keeps two would-be-joined neighbours apart with U+200B.
  - Width: a mark that every terminal gives a column (general category not
    Mn, Me or Cf: Bengali and Tamil vowel signs, the halfwidth voiced marks, a
    skin tone) adds its columns, and alone has a cell; a wide nonspacing mark
    (kana's voiced marks) adds none. The category is read from the comments
    of GraphemeBreakProperty.txt, so no sixth file is pinned. vim and bash
    measure these as two columns; fux gave one and left stray text.
  - Placement: what joins a character that found no room is dropped with it,
    not put on the character before; a character that grows wide in insert
    mode puts a column in; one that wraps on growing gives the last cell back
    what it had; a picture under the second column goes; REP's copies are
    each a character; inserting at the second half of a wide character no
    longer blanks the cell before it.
  - Viewer: a cell cut at a tile's edge or by a menu loses all its parts; the
    span sent again only grows within a row; a wide emoji is drawn over two
    blanks (a terminal with older tables draws a new one narrow); every cell
    that is not plain ASCII is placed by row and column; the viewer's own
    text is cut into characters by the emulator.
  - Not done: a terminal set to draw ambiguous characters wide (the README
    says so); the plain `screen` cannot tell two neighbours from one
    character (it is text; the README and the code say so).

- **D53. A resize wraps the main screen anew** (the panel, 2026-11-19). A
  row that goes on in the next carries a mark on its last cell (two bits
  beside the looks in a cell's style: "the row continues", and "this cell is
  the gap a wide character left"). The mark travels with the row through
  every scroll and is gone when the cell is erased or written, so no second
  list of rows has to be kept in step. `reflow` is a function from the old
  rows and a width to new rows: lines are joined, cut again at the new width
  and never inside a character; the places that must stay on their character
  (the cursor, the saved cursor, each picture's corner) go in and come out.
  `resize` builds both new screens and only then puts them in place: a size
  it refuses changes nothing (the holder asks the terminal device first, so
  a size that refuses changes nothing either).
  - The line of the cursor counts as reaching to the cursor, so a cursor in
    the blank part of a row comes back to its column when the width does. A
    saved cursor there, or on an empty row, is only kept inside the screen.
  - History holds whole lines: a row that goes on is written without its line
    end. What went to history stays there; a taller screen gets blank rows at
    the bottom (tmux, xterm and the old emulator bring rows back; history
    here is a file that other programs read).
  - The alternate screen is cut or padded and nothing of it goes to history.
    While it is shown the main screen behind it is wrapped all the same, so
    leaving it gives the main screen at the new size.
  - Tests: the six properties over 400 runs of random text and sizes from
    fixed seeds; the 120-character line that lost 40 characters loses none.
  - The start: the viewer gives a key script `FUXN_ROW` and `FUXN_COL`, the
    size a split of the focused pane would give the new tile (it splits a
    copy of the layout to know), and the split keys pass it to `fux start`
    as `COLUMNS` and `LINES`. A test's program notes the size it starts at:
    its tile's. The holder still draws what a program wrote before a resize
    at the old size first; nothing waits or retries.

- **D54. The fix check of D53 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  Both reviewers traced the same root: the mark was on the wrong row. "This
  row goes on in the next" stayed true of a row after the next one was
  erased, deleted or scrolled away, so unrelated rows were joined at the next
  resize and in history (`clear` after long output made a line that never
  was), and writing a row's last cell by a cursor move ended a line that a
  line editor was only drawing again.
  - The mark is now on the first cell of the row that goes on *from the one
    above* (as kitty keeps it). Erasing that cell ends the line by itself;
    writing over it keeps it; rows that get a new neighbour (insert and
    delete line, a scroll in a region) lose it; a row erased whole is an
    empty line of its own. History leaves a line open when its rest is on
    the screen, and ends it when the next row to arrive does not go on from
    it. The holder rotates `history` where a line ends.
  - A decision, since two of the ordered properties cannot both hold: when
    the text below the cursor no longer fits, rows go to history from the
    top, the cursor's own row among them, and the cursor is kept at the top
    of the screen. The other way (tmux, xterm: cut rows off the bottom)
    keeps the cursor on its character and loses text. Text is not lost; the
    README says when the cursor moves, and the test checks the cursor
    whenever its row is still on the screen.
  - Also fixed: a saved cursor that waits to wrap stays at its row's end, and
    one in the blank part of a row is kept inside the row without waiting;
    behind the alternate screen the saved cursor is the main screen's cursor
    and comes back to its column; the character printed last is carried
    through a resize, so a mark that arrives after it still joins; a picture
    moves with the cells it was over (two of its corners are followed) or
    goes; a row that ends in coloured blanks stays one row and is coloured
    to its new end; one column: a row is never longer than the screen. The
    size named for a split is taken beside the same pane the script splits
    beside. The start-size test waits for the program's note before it
    splits.
  - The tester's largest point: a tile made narrow for a moment (a split
    opened and closed) left half its pane blank, because rows that went to
    history never came back. Now the rows a resize sends to history are also
    kept (cells and all, a screen's worth at most), until something scrolls
    off after them or the screen is cleared. The next resize wraps them along
    with the screen: what fits comes back, history is told how many bytes to
    take off its end (`Out::unhistory`; the holder cuts what waits to be
    written, then the file), and what still does not fit is given to history
    again. At every moment each character is in `screen` or in `history`,
    once. Narrowing and widening back is exact, cursor and history included,
    whenever the cursor's own row stayed on the screen (the test's 300 seeds;
    a test through a pane's files).
  - Kept, and said in the README: what scrolled off stays in history; a
    wide character on a one-column screen becomes a blank; a space printed
    in a row's last column, with the cursor put back on it and a wide
    character printed there, is taken for the gap that character leaves;
    one resize may put more than 4 MiB of rows into the history that waits
    to be written (at most one screen's worth, written at once).

- **D55. A program that asks what a colour is, is told from one file** (the
  panel; message 11). Three of five programs a seat tried wait on OSC 11 to
  choose a theme. A pane has no terminal of its own, so the truth is kept
  where everything else is: `colours` in the session directory, lines of a
  name and `#rrggbb` that a person can write. The viewer asks its terminal
  once at its start (foreground, background, the 256 palette entries) and
  last of all "what are you": that answer, which every terminal gives, ends
  the wait, so there is no timer, and the file is written with whatever was
  told before it (not at all if nothing was). The holder reads the file
  when it has changed and hands its text to the emulator, which answers in
  the order asked, ending each answer as its question was ended, and is
  silent about what it was not told. Two viewers: the last to start wins.
  Setting a colour is taken and does nothing. Checked with real vim and
  nvim: `:set background?` is `light` on a light file and `dark` on a dark
  one; with no file each keeps its own default.

- **D56. The fix check of D55 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  Fixed: an answer from the viewer's terminal that came in two reads could
  be taken for keys (Alt-], Escape, then text typed into a pane). While the
  terminal is being asked, nothing is taken for a key until it is whole and
  a string is an answer; afterwards a string is not looked for, so Alt-] is
  a key at once. The asking ends at the terminal's answer to "what are you"
  or after one second (D55 had no timer; the order asked for a bounded
  wait, and a terminal that never answers must not keep the file from being
  written). The file is written under a name of the viewer's own and then
  renamed (two viewers shared `colours.new`). `10;?;?` asks for foreground
  and background. A FIFO or a directory named `colours` is not opened (it
  froze the holder); the file is told apart by time, length and inode; it
  is found from the pane's own name, not through `..` (a workspace that is
  a link). The emulator's bound on waiting answers went from 4 to 16 KiB: a
  viewer inside a pane asks 258 questions at once and lost the palette's
  upper part. An answer must be three hex numbers. A person's file may have
  blanks and capitals, and its last line for a colour counts. `colours` is
  no name for a workspace or a pane.
  Kept: the holder looks at the file's state before each read of the
  program's output (one system call; not measured as a cost); answers in
  the 8-bit form of a terminal set to send C1 controls are not read.

- **D57. The kitty keyboard protocol, done as the mouse is** (the panel;
  message 11). The program's requests are emulator state: current flags and
  what was set aside, for each screen, 16 deep (the oldest makes room; going
  back past the first is no flags), answered on the pty with nobody watching,
  and a word (`keys5`) on line 1. Keys are made by the owner's terminal, not
  by fux, so "implemented" means "passed on and still understood by the
  viewer": flags 1 (disambiguate) and 4 (alternate keys). 2, 8 and 16 would
  need the viewer to follow key releases and to read every letter as an
  escape code while a menu is open; they are masked out and a program is
  told so by its query.
  - What is in force is what the program asked for *and* the terminal takes.
    The second is one number in the session's `keyboard` file, written by the
    viewer after it asked its terminal (it sets flags with the old ones kept,
    reads them back and sets none: three sequences a terminal without the
    protocol ignores), or by hand. No file: nothing in force, the query is
    answered 0, and the program uses the old keys. The holder looks at the
    file when its program writes and every two seconds, so a pane started
    before the viewer gets its flags when the viewer comes.
  - The viewer sets the terminal's flags to the focused pane's each frame
    they differ (`CSI = N u`), and gives the terminal back what it had when
    it leaves (`CSI < u`, before it leaves the alternate screen).
  - `keys.rs` names the `CSI … u` forms (and the old forms with the
    protocol's colon parts), so `C-b` and every key file are found in either
    form; a key with Super, Hyper or Meta held, and a key that is no
    character, has no name and goes to the program.
  - Found by hand with nvim, not by the first tests: a pane on its alternate
    screen lost its flags when the viewer read its line 1 back. The pane test
    now asks from the alternate screen.

- **D58. The fix check of D57 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  - A key had two names: Ctrl-Alt-b was `M-C-b` from the old bytes and
    `C-M-b` from the new, so a key file was found in one mode and missed in
    the other. Held keys now have one order in both; a sign that needs Shift
    is named by the sign when the new form says which it is (flag 4); a
    letter of another alphabet is named by the standard-layout key the new
    form gives, as the old form's Ctrl byte names it; a capital is Shift
    and the small letter; the keypad's keys have the names of the keys they
    double. A test states the property for pairs of old and new bytes.
  - The terminal's mask was applied when flags were read, so a program told
    "0" got new-form keys two seconds after a viewer came, and a second
    viewer on a lesser terminal took the flags from under a program told
    "1". The mask is now applied when the program asks: what it was told
    stays true until it asks again. The protocol has no way to tell a
    program that its flags changed.
  - `CSI < 0 u` goes back nothing (the count left out is one); a flags
    number past 255 no longer became 255. The detach test now gives the
    terminal flags of its own and asks for them after the viewer has left.
  - The viewer keeps listening for its terminal's answers until the answer
    to "what are you", ten seconds at most, writing what it has after one:
    answers that came late on a slow line were typed into the pane.
  - Found by the tester on the way, outside this change: a pane split off
    one that had been resized got share 1 beside shares of fifty, a sliver
    of one or two columns. A new part now takes the share of the part it is
    put beside.
  - Kept, and in the README: a viewer killed by a signal leaves the
    terminal's modes on (no signal handlers: it would be the only place fux
    catches one; `reset` puts it right); keys typed in the same read as a
    key that moves the focus reach the next pane in the old pane's form;
    under flag 1 alone a shifted sign cannot be named by its sign; `C-b C-b`
    sends the byte 02, which a program reads as Ctrl-B in either mode.

- **D59. No screen header: one fact, one file** (the owner, 2026-11-20; a
  panel of eight found nothing that breaks). `screen` and `screen.styled`
  are text only. `size`, `cursor`, `modes`, `state`, `top`, `wraps` and
  `pictures` are files, each written when it changes; `status` has the
  program's number, once. What the panel's attempts to break it require is
  kept: facts are written before the screens and `screen.styled` last, the
  screens are written in every round (a round is only made when something
  changed), `fux wait` is the signal for a program's end, and the viewer
  takes its rows from the text and keeps the cursor inside them. `v=` is
  gone with the header: nothing counted on it but one test, which now counts
  replacements of `screen.styled`.
  - Mine to decide: `cursor` gives the shape as the number a program sets
    (0 to 6), not a word; `pictures` gives rows of the screen, as the header
    did (they are written before the screens; marks and links, when they
    come, are numbered from the pane's first line with `top`); a fact with
    nothing to say is an empty file, not a missing one, so `read x < file`
    never fails on a living pane; a row that goes on from the row above is
    told by `wraps`, a line of row numbers, and the editor key joins such
    rows as history does; `top` counts whole lines in history (a line whose
    rest is still on the screen is not yet one), and goes down when a
    resize takes rows back.
  - Met on the way and fixed: OSC 52 with padding that does not fit its
    length is refused (it wrote a NUL); a keyboard-flags number too large to
    be one is no flags (it was taken as 5); a viewer removes a dead viewer's
    FIFO of its own name before making its own.
  - `sizes.sh` prints and no longer fails (the owner: "I never really said
    I want one").

- **D60. The fix check of D59 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  Fixed: `status` was written after the first screen (the TERM key could
  read the new `state` with the old program's number): it is a fact like the
  others now, written with them, and what an earlier program left is removed
  before the first screen, not after; `fux start` returns when the screen is
  there too. A round of writing that fails is tried again (it was dropped
  until the next output). Wrapped rows keep their blanks in `screen`, and
  the editor key puts a line end between an unfinished line of history and
  a first row that does not go on from it; `top` counts that line. The
  viewer shows anything but text and colours in a `screen.styled` as `?`,
  and cuts a line longer than `size` says cleanly (it drew `abcdP` for
  `abcdefghijKLMNOP` at 5 columns). `fux wait` opens `state` and `status`
  without waiting. 65535 is flags, 65536 is not. The TERM key asks after
  the holder before it trusts `running`.
  Kept: if the very last round of writing fails, `fux wait` reports what is
  on disk (the holder's `log` says why); lines that were dropped because 4
  MiB of history were waiting are not counted in `top`; a FIFO left by a
  dead viewer of the same number is removed, untested (the number cannot be
  chosen in a test); the order of writes is tested by the files' times on an
  ended pane, not by intercepting each write.

- **D61. Small facts and modes** (message 12, step G; the panel's forms).
  - `cwd`: the path from OSC 7, kept if the host named is none, `localhost`
    or this machine's (`gethostname`, one more declared call), removed if it
    is another's. The split and new-tab keys `cd` there before `fux start`.
    A path that is not UTF-8 is not told.
  - `notice`: `NUMBER WORDS`, replaced whole; the number alone is the bell
    (one file, not a `bell` beside it: one thing to watch). `notify TEXT` on
    `ctl` for anyone. The viewer looks at all panes' notices four times a
    second, rings its own terminal once for a new one, says it in the
    corner and marks the tab in the bar until the pane has had the keys.
    What was there at its start is not rung. No log.
  - Mode 2026: the holder makes no round of writing while the program says
    it is drawing, for half a second at most.
  - Mode 1004: a word in `modes`; the viewer tells the pane that gains or
    loses the keys (if it asked), and asks its own terminal for the same so
    that the window's focus reaches the program too.
  - Mode 2027: the viewer asks once; if the terminal knows it, it is turned
    on (and off again at the end if the viewer turned it on) and no `??` is
    drawn. A pane of fux answers "set for good", so the pane test now sees
    the character itself at a tile's edge; the stand-in is covered by the
    probe with a Ghostty engine only (not in CI).
  - Underlines: kind (double, curly, dotted, dashed) and colour are in a
    cell's style and in `screen.styled`, always in the form with colons,
    which a terminal that does not know them skips whole.
  - Pickers are key scripts (`C-b f p|t|w`) and one helper, `.pick`, run in a
    pane. One thing a script cannot know: which pane the viewer itself runs
    in. The viewer says it (`FUXN_SELF`), and the workspace picker leaves
    that workspace out.

- **D63. The fix check of D61 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  Fixed: a pane whose program had turned focus reports off (vim leaving)
  was sent `CSI O` into its shell: the pane that loses the keys is told only
  if its `modes` still asks. The terminal's own `CSI I` and `CSI O` went
  through the keys (they closed an open menu and could reach a pane that
  never asked): they are read as what they are and passed to the pane with
  the keys only if it asked. Two frames drawn back to back within the half
  second were shown half way: each "I am drawing" starts the half second
  anew, and the holder sleeps until then instead of waking every
  millisecond. A notice's words are passed on with the pane's name before
  them, so they cannot be read as a numbered OSC 9 command; OSC 9 with a
  number first, and OSC 99 questions, parts and encoded text, are no notice.
  The underline's kind and colour are a second code after the plain one (a
  terminal that drops a code with colons dropped the reset and the colours
  with it). `cwd`: 4,096 bytes at most, no U+FFFD, and a far machine named
  with a bad path still removes the file; host names compare without case.
  `notify` on `ctl` may be as long as a notice (the line bound is 1,280).
  The viewer's memory of notices is kept across a change of workspace and
  cleared for the pane with the keys at every frame. The tab finder names a
  pane of the tab (a number went stale if a tab closed meanwhile); the
  workspace finder compares directories, not their names' spelling.
  Kept, and in the README where a user meets it: only the shown workspace's
  notices are looked at; of two notices within a quarter second the last is
  passed on; a failed write of `cwd` or `notice` is logged and not tried
  again; the child may write `cwd` itself before the holder has cleared an
  old one (a restart keeps the last program's files until the new one is
  up); a soft reset leaves modes 1004 and 2026.
- **D62. Prompt marks are line numbers of the pane** (message 12, step H; 6
  of 8 seats). OSC 133 A, B, C and D are kept as the number of the line the
  cursor is on, counted from the pane's first line: `top` plus the lines
  above the cursor on the screen (rows that go on from the row above are no
  new line). A line's number does not change when it scrolls into history
  or is wrapped anew, so `marks` is written only when a mark is added. (What
  a new prompt above a mark does is in D64.) The last thousand; none on the alternate
  screen.
  - With `top`, a mark is a line of history plus screen: `.text` writes that
    text into a file and says how many of its lines are before the screen's
    first; `.edit` opens a file in a tab. `C-b c` is those two; `C-b C` cuts
    the lines from the last `output` mark to its `end`; `C-b p` opens the
    text at the prompt before the last. The keys are mine to choose (the
    order named none): `C` beside `c`, `p` for prompt.
  - Kept: a line older than `history.1` is in no file, and the keys then
    start at the text's first line; insert and delete line above a mark
    move the text and not the mark.
- **D64. The fix check of D62 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  All three found the last command's output wrong when it was empty or did
  not end its line; the two reviewers found marks paired wrongly after a
  clear. Changed:
  - a mark is `LINE COL KIND [EXIT]`: with the cell, `printf hello` is
    `hello` without the prompt after it and `true` is nothing, which the key
    says ("the last command printed nothing") instead of opening the prompt;
  - marks are not dropped any more. A prompt above marks, and a clear of the
    whole screen (`ESC[2J`, so Ctrl-L and `clear`), bring the marks below to
    that line, in their order: an output and its end stay a pair, of nothing,
    and are never paired with another command's. Only a prompt does it: a
    program that moves up and ends has erased nothing (sol's case). Only
    the whole words `A`, `B`, `C`, `D` mark;
  - both keys refuse while the pane shows the alternate screen (`alt` in
    `modes`): the marks are the shell's and the text is not;
  - `.text` reads `top` itself, before and after the text, and reads again
    (five times at most) if it moved; it writes with umask 077; the keys say
    when the output is older than the files or no prompt is marked, and
    remove their file when they stop.
  - Kept, and why: a publish that lands between `.text` reading `history`
    and `top` in the one moment the holder has written the first and not the
    second still gives lines off by what scrolled (there is no stamp that
    covers several files; D59 chose files over one header and this is its
    price, for a key pressed on a pane that is printing); lines dropped at
    the 4 MiB bound of unwritten history are not counted in `top` (a disk
    that stalled that long has lost the text too); an unfinished line that
    is only in `history.1` after a rotation and two resizes is joined to
    the screen's first (three rare things at once); insert, delete and a
    scroll of part of the screen move text and not marks (now in the
    README's row); `EDITOR` is split at spaces as before. sol's wish for
    tests of each of these was met for the cases that changed (empty output,
    output without a line break, a clear, a mark above marks), not for the
    kept ones.

- **D65. The whole kitty keyboard protocol** (message 12, step I). All five
  flags are passed on (the mask is 31), still only as far as the terminal
  takes them. The viewer's part is what flag 2 and 8 make necessary:
  `keys::event` reads a key's number, whether it is pressed, repeated or
  released, and whether it is a modifier by itself. The viewer remembers
  which keys are down: those it took (prefix, menu keys, keys a menu
  swallowed) and those it passed to the pane with the keys. A release of a
  key it took goes nowhere; a release goes to a pane only if its press went
  to that pane (the list is cleared when the keys move to another pane);
  with a menu open, releases and lone modifiers do nothing.
  - Mine to decide: a repeat of a key the viewer took is dropped, except in
    a menu that stays open (`.repeat`), where a held key acts again: without
    flag 2 a terminal sends a held key as presses and the resize menu
    repeats, and a program's asking for flag 2 should not change that.
  - Under flag 8 a plain letter comes as `CSI 97 u` and is named `a`: the
    menus work as before. A shifted sign is named by its sign only if the
    terminal says which it is (flag 4): as before.

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
