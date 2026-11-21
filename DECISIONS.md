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
    says instead of opening the prompt;
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
- **D66. The fix check of D65 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  All three found keys left down in programs: D65 dropped a release when
  the keys had moved or a menu was open, and with flag 8 the prefix's own
  Ctrl goes to the pane first, so every use of the prefix left Ctrl down
  there. The order's rule ("never a release to a pane that did not get the
  press") does not ask for that. Changed: one list of the keys that are
  down, each with where its press went. A pane's key stays that pane's: its
  repeat and release are written to that pane's `in`, wherever the keys are
  by then and with a menu open; not if the program there is another one
  (the pid in `status`), so a pane started anew, or another workspace's of
  the same name (astra), gets none. A press takes its key out of the list
  first, so a key the viewer took whose release never came (no flag 2 then)
  cannot swallow a later release (Opus). The list keeps the newest 64, not
  the oldest. The locks count as modifiers. A repeat acts again in a
  `.repeat` menu only if its press ran a script there (holding the key that
  opened the menu closed it). A key's sequence may be 1,024 bytes before it
  is given up (astra: a key with much text cut by a read after 64 bytes
  became two keys).
  - Also: a character by itself is a press of its key (a terminal that
    sends a letter's press as the letter and its release as a sequence, as
    wezterm's encoder does, has its release passed on); an event type that
    is none of the three is nothing; a program may clear a flag the
    terminal no longer takes (astra: the mask was put on what is cleared
    too); the keypad's signs are named as the signs.
  - Not done: a press that the pane's full `in` refused is still owed its
    release (astra 6); two keys with one character and different base keys
    held at once are one key to the list (astra 10); `ESC [` alone after a
    pause is still `M-[` (as before this step).
  - Kept: the keys go to a pane as the terminal sends them, so while a menu
    is open they still come in the form the pane asked for; `C-b C-b` sends
    the byte 2 to a program that asked for escape codes (it reads it as
    C-b); a pane whose press was seen before its pid was known (the first
    frame) gets no release.
- **D67. Links are a file of runs, and the screens stay text** (message 12,
  step J; the panel's form). A cell has the number of its link (two bytes;
  the addresses are in a table of the emulator, a thousand at most, the
  unused making room when it is full), so a link goes where its cell goes:
  scroll, insert, reflow. An erased or overwritten cell has none. The holder
  writes `links`, `LINE COL LEN ADDRESS` for each run on the screen shown,
  LINE as `marks` counts it; `screen.styled` has no OSC in it, as D59 said.
  The viewer puts the runs back on its copy's cells with `top` and `wraps`
  and wraps them in OSC 8 when it draws, opening and closing with the cell,
  before the blanks a wide character is drawn over.
  - An address is printable ASCII without blanks, 2,048 bytes at most,
    checked by the emulator and again by the viewer (the file is anyone's
    to write): it is the one thing of a program's that the viewer writes to
    the terminal inside a control string, so nothing that could end the
    string or start another gets there. A URL with other bytes is no link;
    programs percent-encode.
  - Mine to decide: the `id` parameter is dropped. With it a terminal
    lights up two runs as one link; the viewer would have to make ids that
    differ between panes and it draws rows apart anyway. Links are of the
    screen shown, the alternate one too (a pager's links work), numbered
    on from `top`. History has no links: it is plain text, and the order
    said so.
  - Kept: a link is not carried through `fux view` inside `fux view` any
    differently (the outer pane's emulator reads the inner viewer's OSC 8
    as any program's); the viewer's table of addresses is emptied and all
    drawn anew past 4,000.
- **D68. The fix check of D67 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  All three: nothing but an address's own printable bytes reaches the
  terminal. Found and changed:
  - a cell could come back with another link's address: making room in a
    full table looked only at the two screens, not at the rows a resize set
    aside nor at the cell kept under a character being joined (both
    reviewers, traced). Both are looked at now; a character being joined is
    forgotten when the screen is switched (it could be put back on the
    other screen);
  - an address with a control byte in it became the address without it (a
    string drops such bytes): no link now (tester, sol);
  - the list: the viewer read 256 KiB and a thousand lines, the holder
    wrote without bound; a cut record's address was taken short, and a
    screen of single-cell links with long addresses was half a gigabyte
    (sol). The holder's list ends before the run that would pass 256 KiB,
    the viewer takes whole records only, a thousand addresses and no more
    cells than the screen has;
  - the viewer: a line number from `top` is added to without overflow; the
    link is closed before every cursor move, so none is open across a line
    between tiles when only the cells on both sides changed (sol), and when
    the viewer leaves (both); its table of addresses is emptied only when
    most of it is of links no longer drawn (five panes of a thousand links
    were drawn anew every frame), and an address is looked up once for a
    pane and a frame, not once for a cell; past 65,535 addresses on one
    screen a cell has no link; a number with no address is no link;
  - a blank that is part of a link is text to reflow (the link on a space
    at a line's end was lost); a soft reset ends the link being printed, as
    it ends the colours. Entering and leaving the alternate screen do not.
  - Kept, and why: `links` of one publish read with `screen.styled` of the
    one before puts, for one read, a new address on old text (D59's price
    again: no stamp covers several files; the README's row says it); the
    viewer's copy has no room for a 500th column's last character (as
    before this step, now seen as a link on a blank); the 256 KiB list
    holds 127 runs of the longest addresses, thousands of ordinary ones;
    a thousand addresses are "at a time" as the program prints, so text
    that is about to scroll still holds its own (the tester's count).
- **D69. The second look at the night's work** (message 13: four reviewers by
  area and two README-only testers on the whole of D59 to D68; one fix
  series). Fixed by the rule behind each group; where an earlier entry says
  otherwise, this one is the newer.
  - **A mark is on its text** (their findings on marks after a clear, an
    erased row, a wrap, history, wide characters). On the screen a mark is a
    row and a column, and what is done to the rows is done to it: scrolled,
    inserted, deleted, it moves with its row; what is erased (`ED`, a
    reset) takes its marks to where the erased part begins, in their order;
    wrapped anew, it is a place in the text for the while. When its row
    goes into history it becomes a line's number and bytes into the line.
    `marks` is `LINE AT KIND [EXIT]`, AT in bytes of the line's text, the
    part history has of the line counted (history's unfinished line keeps
    its length for that) and the cell a wide character left empty at a
    row's end not. Bytes, not cells or characters: a script cuts text, and
    `cut -b` and `LC_ALL=C awk` cut bytes the same on every system, never
    inside a character, where cells need a table of widths no shell has.
    The last-output key cuts so; equal marks are "nothing here". This
    replaces D64's `COL` in cells and its clamping by line number, and
    D62's "insert and delete move the text and not the mark". `links` stays
    in cells: it is for drawing.
  - **History's last line ends when what it went on in is erased**, then and
    not at the next scroll; `screen.styled` keeps the blanks at a wrap as
    `screen` does.
  - **Mode 2026's half second** runs from the first begin with no end since
    (D63 started it again at every begin, which a program that repeats the
    begin could use to never be shown; two frames back to back still have
    a clock each, since an end lies between them).
  - **Everything a publish writes is a fact**: title, clipboard, directory
    and notice go through the same written-if-changed list as the others,
    so a failed replacement of one fails the round and is tried again, and
    the same directory said twice is written once (a clipboard set again
    is written again: the terminal's may be another's by now). The last
    publish is tried for two seconds before `fux wait` can return.
  - **A new pane's first notice is new** if it was written after the viewer
    first looked at the workspace (by the file's time, with 20 ms for the
    clock's grain); one that came with a pane moved in is old, as before.
  - **A helper's file is in its pane's folder** (`.edit`'s text, `.pick`'s
    list: moved there by the pane's own program), so it goes with the pane
    however that is closed.
  - **How the editor takes a line is a setting**: `FUXN_EDIT`, a command
    with `%f` and `%l`, read by `keys/.edit`; `hx` and `helix` are known
    (`file:line`). The owner's `EDITOR` is a launcher, which hides what it
    runs; it was not run here, so whether it passes `file:line` on to
    helix is his to try.
  - **Keys held and let go** (the keyboard reviewer's four, each proved by
    a failing test first; a helper's work): when the keys leave a pane
    with a key of its still down, the viewer itself tells that pane the
    key is let go, and what the terminal still tells of that key reaches
    no pane. Not "keep the terminal's flags while a key is down": that
    waits for a release that may never come (a modifier held as the window
    manager takes the keys), and a plain shell would get escape codes
    meanwhile. Who a pane's program is, and whether it asks for releases,
    is read from its files when a key is passed, not remembered; only a
    press makes a key someone's; a held key runs again the script its
    press ran, in that menu only; a sign pressed as itself and let go by
    its key's number is one key. This replaces D66's "wherever the keys
    are by then" and its "pane whose press was seen before its pid was
    known".
  - **The viewer draws every cell as its own character** (links, a helper's
    work): a cell that a terminal would join to the one before it is sent
    with U+200B in front, as `screen.styled` has it, so two linked
    characters side by side keep an address each and nothing joins the
    line between tiles; any link is closed before the first frame; the
    table of addresses keeps the ones on the terminal and numbers them
    anew when it must (D67's "emptied and all drawn anew" is gone); the
    copy has its extra column only where there is room (D68's 500th
    column); the list of links ends at the first run that would pass the
    bound.
  - Small: the terminal's report modes are sent when they change, not with
    every frame; focus reports follow the window (a pane is not told twice
    that the keys left, nor that it has them while the window has not);
    a number typed at a numbered list is a line first and a name only if
    no line has it; the TERM key works on a program that was hung up and
    still runs; OSC 7 with `127.0.0.1` or `[::1]` is this machine; the
    keys test changes the layout by the command and says what the viewer
    showed when it waits in vain (the CI failure seen once was not
    reproduced; this removes the likelier cause).
  - **Not fixed, and why** (each in the README where a user would look):
    - bash draws a long pending command twice after its pane is made
      narrower. Neither the reflow nor the size it is told is wrong: bash
      clears and redraws counting rows up from the cursor as if nothing
      had been wrapped anew. tmux 3.8 on the same steps shows the same two
      copies whenever the prompt is not on the screen's first rows (there
      its copies happen to be in history). Not wrapping the cursor's line
      would lose text at a narrowing, which is worse.
    - a line feed onto a row that goes on from the one above does not end
      the line above (a program that writes `first\r\nLAST!` over a
      wrapped line leaves one line). bash moves down inside a wrapped
      command with a line feed, so ending the line there would break
      every long command at the next resize; tmux keeps the wrap too.
    - a long output cut in two by Ctrl-L or `clear` opens as the part that
      had scrolled into history: that part is still the output's, and the
      rest is gone from the pane.
    - a line that never ends passes `history`'s 4 MiB and is cut at 8 MiB
      (by design: a file ends where a line ends).
    - a release the pane's full `in` refuses is lost; a key's sequence cut
      by a read after 1,024 bytes is two keys; a held letter is typed into
      a next pane that asked for no flags (the terminal sends plain
      letters then, as without fux); more than 65,535 different addresses
      on one terminal leave the surplus unlinked; ten notices in one write
      are one replacement of `notice` (the number says how many); a cursor
      move away and back does not part a character from a mark printed
      after it; the files of a pane are read one after another (D59).
- **D70. The fix check of D69 (1 Opus, 1 gpt-6.2-astra; one pass).** Opus
  found no must-fix; astra twelve, by tracing. Changed:
  - marks: a mark that waited at a row's end where a wide character later
    left the last cell empty counted that cell (the cut landed inside the
    character): a mark is never past its row's text. `ED 1` takes the mark
    right after what it erased too (an output's end survived on new text).
    A mark at a row's end stays on that side of the wrap through a resize
    (it moved to the next row's start, and an inserted line then parted it
    from its text). Marks on rows pushed off the bottom (insert line,
    scroll down) are after what stays, not before it (both reviewers).
  - the viewer: a window's focus report goes to the pane that was told it
    has the keys, not to whichever the layout names by then; a key typed
    means the window has the keys (the window could stay "without" for
    good after a program stopped asking while it was away: Opus); a
    release is owed only to a program that asked for releases when it got
    the press (a letter typed to a shell was "let go" in the program
    started there later), and never to a program whose number could not be
    read; the link table's renumbering cannot overflow at 65,535.
  - the holder: the directory is written whenever the program says it (the
    file is the program's to write too, so "same as last time" is not
    known); a missing file ends the holder only if the pane's `in` is gone
    as well; a full reset does not start the 2026 count again.
  - scripts: `.edit` puts the line in before the file (a path with `%l` in
    it was changed); a helper's file that is in its pane's folder is not
    removed by name when the helper ends (the folder goes; a moved pane's
    old name may be another helper's by then); the picker compares names
    as text (`010` and `10`).
  - `marks` is written with one walk over the screen however many marks
    are on it (astra: a thousand marks at the end of one wrapped line on a
    500 by 500 screen walked it a thousand times at every publish).
  - tests: the layout file is replaced in one step and a new pane's place
    is waited for (the CI failure of this series' first push: a plain
    write read half-way by the viewer, which then made a layout of its
    own); the close test waits for the pane's folder to go; the fixture
    that printed after a second waits for a file instead.
  - Not done, and why: a wide character that a one-column screen turns
    into a blank changes the bytes of its line, and a mark after it is
    then two bytes late (a one-column pane is no place to read output);
    the first megabytes of a line that never ends are cut from `history`
    at a forced turn-over, and marks in that line then count from a start
    that is gone; a last screen that cannot be written for two seconds is
    given up with a line in `log`, and `fux wait` then prints what is on
    disk (a FIFO planted as `state.new` blocks the holder, as it did
    before this series); rows a resize set aside do not come back once
    the row they went on in was erased (history's line was ended there);
    lines dropped at the 4 MiB bound of unwritten history lose their
    marks' numbers (D64); blank lines at the very end of an output with an
    empty prompt after them are not in the copy; a notice written after
    the viewer's first look that comes with a pane moved in is passed on
    once; an editor whose path has a space cannot be named in `EDITOR` or
    `FUXN_EDIT`; panes started before this series have `marks` in cells
    (pre-release: start them again); the TERM key asks after the holder's
    number a moment after it read the program's; a paste typed in the
    instant the focus moves is wrapped as the last pane asked.
- **D71. After the last two testers** (message 14; a working day, and
  scripts and agents). One small series, no review round.
  - `history` is put aside at the first line end after it has passed 4 MiB,
    wherever in the lines to be written that end is: the lines up to it go
    to the old file, the rest to the new. Before, the holder looked only at
    whether the file ended with a whole line at the moment it wrote, and a
    stream of wrapped lines (three rows a line, a row or two scrolled off at
    a time) seldom ends a write on a line's end: the file grew to 8 MiB,
    where the rule for a line that never ends cut it inside a line. After
    rows are taken back from the file (a resize), whether it ends with a
    whole line is read from its last byte, not assumed.
  - When keys come, the viewer reads who has them from the layout (D69) and
    now also lets go, in the pane that had them, the keys still down there:
    a repeat that comes before the next frame reached the pane that had
    just lost the keys.
  - `FUXN_EDIT` is passed on to a pane's program with `FUXN_DIR` and
    `FUXN_KEYS`: a viewer started in a pane is a viewer like any other.
  - In a menu that stays open, the key that led there (the prefix) opens
    its own menu again instead of being swallowed, as in fux 1
    (`keymap.rs`, `repeating`: "the key that led here opens its column
    again"). Any other key the menu does not have still ends it and goes
    nowhere.
  - `.shell` has its workspace's layout follow when its pane's folder is
    removed, so a workspace left through the workspace picker has no line
    for the picker's pane. The pane and tab pickers show the tab shown
    before their own first, so that it is the one `fux layout tab -` goes
    back to after the choice.
  - Left: after the tab shown is closed, the tab shown before it is shown
    and no tab is marked as shown before that one (the layout remembers
    one tab back, not two; a second mark would be a new word in the file).
    The resize keys make the focused pane narrower and wider rather than
    move a border, as before. A viewer killed outright leaves its FIFO and
    the terminal's modes (README).
- **D72. `fux help agents`** (message 15; the owner asked for it). One page
  of plain text in the binary (`src/help-agents.txt`), for a reader with
  the binary and nothing else. Its indented lines are one shell session
  that `tools/help-agents.sh` runs as written, in CI on both systems: a
  command on the page that stops working fails the build, so the page
  cannot drift from the program. Hence its shape (examples that follow
  one another, loops with a limit, no `timeout`, which macOS lacks) and
  what it leaves to the README (the viewer's keys, `marks` and `links` in
  full, pictures). There is no command to wait for output: the page gives
  the loops, since what "settled" means is the reader's to say.
- **D73. The agents' page after the first blind test** (message 16: an agent
  with the binary and the page alone did 25 of 27 tasks as asked). The
  page changed where its record shows it guessed or was misled; no new
  subcommand (the eight it proposed are the owner's to decide).
  - Whether a pane is alive is the lock on its directory, which the holder
    holds while it lives: not `state` (a crashed holder leaves `running`)
    and not the holder's pid (a zombie answers `kill -0`, which the old
    check used). A write that cannot hang is an open of the FIFO without
    waiting, which fails at once with no reader; sh cannot do either
    (POSIX has no such open and macOS no flock(1)), so the page gives both
    as one line of perl, which Linux and macOS have, names python's form,
    and says what each failure means. fux could make them one line only
    with a new command; that is with the owner.
  - Waiting is three small functions on the page (a text seen, a frame
    changed since before the key, the screen settled), since "two equal
    reads" alone can be the old frame twice.
  - Each fact file has its line with an example; `whole` is stated as
    exact but for four things; renaming with `mv`, the viewer in a pane
    of another workspace (the page had said not to run it), a program that
    ignores the hangup, and "two above, one below" are run on the page.
  - `tools/help-agents.sh` runs the lines indented by exactly four blanks
    (52 now), with a keys directory of its own, and checks that the
    session holds nothing after the page's cleaning up but the two files
    the page says a viewer leaves.
- **D74. The agents' page after the second blind test** (message 17: 26 of
  27 as asked, on another model). The owner's rule: no command for what a
  script recipe can do, so `send` and `alive` stay recipes on the page.
  The page only: every wait recipe returns non-zero at its limit
  (`settled` did not) and the page says to check it; `fux wait` and
  `state` are about the pane's program, not what it started, with the
  process group as the way to see and end the rest (the hangup example
  no longer hides this behind `exec`, and ends with a command after
  `sleep`, since bash execs the last command of `sh -c`); `fux dir` names
  a directory that may not be there yet; a viewer in a pane has what it
  passes on in its own `notice`; `colours` only if the terminal answered;
  `alive_sh` for a machine with neither perl nor python, with its limit.
- **D75. The agents' page in sh alone** (message 19; the owner: "pure sh
  only", and no command for what a recipe can do). The perl recipes of D73
  are gone. A write that cannot hang is `1<>` on the FIFO, as the key
  scripts write: it never waits on the open, and on a dead pane the bytes
  are lost without a word, which the page says. Alive is the holder's
  process, there and not a zombie (`ps -o state=`), with its one limit (a
  number used again after a crash); `flock -n` is named as exact where
  Linux has it. What was given up: a write that reports a dead pane by
  itself, and an exact test on macOS. Both are run in the page's script
  against a holder killed with -9.
- **D76. sh by default, perl as a documented alternative** (message 20; the
  owner's words). The page teaches the sh forms and every example uses
  them, with their three risks said where they are taught. A section near
  the end, marked optional, has the exact forms for who has perl: `send`
  (fails at once and says why) and `locked` (the directory's lock), and a
  line for python. The page's script runs that section only where perl is
  found, and checks that nothing outside it names perl or python. The
  README's zoom-back script is treated the same: `fux layout` needs
  nothing; the perl line is for a hand edit under the lock.
- **D77. The agents' page after the split blind test** (message 21: one
  reader without perl or python, one with perl; 30 and 31 of 32, neither
  ever hung). From what they got wrong:
  - waiting for a command's end in `marks` is waiting for one more `end`
    than before the typing (`count`, `more`): the example had matched the
    `end 0` its own shell writes before the first prompt. The example
    shell now marks `prompt`, `command` and `end`; `output` needs PS0,
    which macOS's bash 3.2 lacks, and the page says so.
  - every wait takes its limit as an argument, and the page says in
    capitals to check the result; `ended` bounds `fux wait` (the holder's
    going, with a limit) in sh on both systems.
  - "a write waits while 64 KiB are queued" was not reachable as said and
    is gone. What is true, tried here: under a program that reads lines
    or nothing, the terminal keeps about 4,000 bytes of a line (cat got
    4,096 of 6,001) and drops the rest, so 300,000 bytes never waited;
    only under a program in raw mode that has stopped reading do they
    queue, and a writer waited after some hundred KiB.
  - an idle viewer draws no bar and no tab names (they come with the
    prefix); a bell passed on is a bare number in the viewer pane's
    `notice`; a loop lists every workspace and pane; `fux help` ends by
    pointing to this page and no longer to a README.
  - Cut: automatic numbering, the application cursor keys, the clipboard
    line, the layout edits beyond split, focus and tab (named only). The
    page is 222 lines against "near 200": the new recipes are long lines
    of their own, and nothing else was the testers' to spare.
- **D78. The agents' page, two wordings** (message 22, after a fifth blind
  reader did 31 of 32 with nothing of the page in its way): beside the
  first typing example, what counts as evidence that a command ran (what
  only the program can produce, never the typed words, which the echo
  puts on the screen too); and that a wait's limit counts tries, not
  seconds.
- **D79. fux answers as fux** (message 23, step 1; the panel, 8 of 8).
  XTVERSION says `fux-next VERSION`. XTGETTCAP answers from one table
  (`CAPS` in `term.rs`; the README's table is held to it by a test), each
  name by itself, "not valid" for every name not in it. DECRQSS tells what
  the emulator keeps: SGR, the scroll region, the cursor's shape. The
  pane's program has `COLORTERM=truecolor`. `TERM` stays
  `xterm-256color`: fux installs no description, and `TN` says what
  `TERM` says.
  - The second device attributes stay `1;10;0`: class 1 as kitty, foot and
    VTE give, and a version (10) below every number a program tests for
    (vim wants xterm's 141 and more, tmux is 84, screen 83), so it is no
    other terminal's and turns nothing on. A new number would be one more
    thing for programs not to know.
  - In the table only what the emulator does; `Ms` because setting the
    clipboard works (reading it is refused, as before).
- **D80. The fix check of D79 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  - The second device attributes are `70;0;0` now: `1;10;0`, which D79
    kept and called no other terminal's, is Ghostty's (sol, from the
    panel's own record). 70 is a first number no terminal known here gives;
    a program that goes by the number finds no terminal it knows.
  - `Ms` is out of the table (both reviewers): the name promises a choice
    of clipboard, which fux drops, and Neovim takes it as a clipboard it
    can also read, and then waits ten seconds at each paste for an answer
    fux refuses to give. Setting the clipboard still works; the README
    says why the name is not there.
  - `Se` is `CSI 0 SP q` (the outer terminal's own shape, which is what 0
    means to the viewer), not xterm's `2`.
  - A question too long to answer name by name is one "not valid" (the
    tester: 33 names got no word at all). The README's table has its
    header, and the test holds it row for row; DECRQSS's answer is fed
    back in the test and must give the same looks.
  - Said in the README, not changed: `COLORTERM=truecolor` is true of the
    pane and not of every outer terminal, and the viewer does not convert
    (few terminals are left without; a conversion table is not worth its
    lines until someone has one); a program that goes by `TERM` alone
    (a nested tmux) may use what an xterm has; an unread answer is typed
    text; SGR 6 is kept as 5.
- **D81. Performance, measured beside tmux** (message 23, step 2; the panel
  6 of 8: keep whole files and the poll). `tools/perf.py` runs fux and tmux
  through the same steps and prints medians and spreads; the README's "How
  fast" has this machine's numbers before and after. Four changes, each
  with its numbers in its commit:
  - the viewer composes no frame when what a frame is drawn from (the
    files' stamps, its own state) is what the last one was drawn from:
    idle 3.2% of a core to 0.1%;
  - the holder publishes as soon as the program goes quiet (nothing more
    within a millisecond), and the viewer looks every millisecond for a
    moment after a key: a key's echo 20 ms to 4 ms typing fast. The bound
    under a flood stays 40 screens a second; a program that writes a
    little every few milliseconds can now be published up to 250 times a
    second, which the README says;
  - what goes to the terminal is written by a thread of the viewer's own,
    so keys are served while a frame drains to a slow terminal: 52 ms
    (up to 264) to 0.2 ms. A thread and a channel from the standard
    library, no reopening of the terminal and nothing per platform;
  - a scrolled line no longer makes and copies a row of blanks, and a
    row's text is read once from its end: `seq 1 2000000` 4.4 s to 1.45 s,
    1.5 times tmux's 0.96 (it was 4.3 times).
  - Left: a key's echo is 2 to 4 ms against tmux's 0.2 (the holder's wait
    for quiet, the viewer's poll, composing a frame): the price of files
    and a poll, which the panel kept. A smaller cell would bring the flood
    nearer; it touches every place that writes the grid and is a change of
    its own.
- **D83. The fix check of D81 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  Opus and the tester found wording only; astra traced six faults. Changed:
  - what input needs of a pane (does it take a paste in brackets, the
    mouse) is read from the pane's `modes` when the input is sent, not
    from what the last frame remembered: with frames skipped while one
    drains, the memory could be as old as the terminal was slow. Whatever
    empties the viewer's memory of panes empties its memory of the last
    frame's inputs too (two workspace commands at once left it empty and
    "unchanged" for good; a test shows it);
  - "quiet" is a millisecond since the program last wrote, whatever woke
    the holder; a quiet publish comes at most every 8 ms (it was 4): 125
    screen pairs a second for a program that trickles, 40 for one that
    never pauses. Typing measured the same at 8 as at 4. The README's
    Bounds say this in pairs of files and say that nothing is synced;
  - an escape prefix waits 4 ms from its last byte, apart from the poll;
    the fast look for an echo follows only a key that went to a pane; a
    slow terminal no longer makes the viewer poll fast (the writer says on
    a pipe when it has written); the cell's pixel size is part of what a
    frame is drawn from; the read budget is filled at each publish;
  - `tools/perf.py` counts every thread of a process, says when samples
    are missing, and cleans up on a signal.
  - Open: the tester saw the bar missing in 1 of 30 samples of the outer
    pane's screen during a flood. Every write that draws cells is one
    string inside mode 2026's brackets; no cause was found by reading,
    and it was not reproduced.
- **D84. Colour in scrollback** (message 23, step 3; the panel 8 of 8; the
  owner: keep both plain files). `history.styled` beside `history`: the
  same lines in the same order, text and SGR only, each row written as
  `screen.styled` writes it (one function), so with the codes taken out it
  is `history` but for blanks at a line's end (a coloured bar stays a bar)
  and the U+200B that keeps two characters apart. `marks` count bytes of
  `history`, as before. Rows taken back after a resize and a line ended
  by `settle` go through the one place that writes both.
  - The two are put aside together, at the same line, when either passes
    4 MiB: on the plain file's size alone, output with a colour at every
    character would let the styled file grow twenty times that in a
    session directory that is often memory. So lines dense with colour
    keep less history.
  - A line without colour costs its bytes a second time and one look at
    its cells (a row of plain ASCII is copied, not put together again):
    the flood went from 1.44 s to 1.51 s.
  - `C-b M-c` pages a pane's text in colour: `.text FILE styled` joins the
    styled files as it joins the plain ones, opened in `less -R` at its
    end through `.edit`. Alt-c: beside `c` (the editor) and `C` (the last
    output), and not Ctrl-c, which people press to back out of a prefix.
- **D85. The first screen** (message 23, step 4; the panel 8 of 8). A
  workspace that has no pane when a viewer first shows it gets what
  `keys/.empty` starts: a shell in a first pane, by default, as the
  new-tab key starts one. A script, so it is the user's to change (a
  project's panes, a restore, nothing).
  - Mine to decide: only when the workspace is first shown (at start, and
    on a change of workspace), not each time it becomes empty. A workspace
    whose last shell ends while it is shown stays empty and says how to go
    on (`C-b t n`): a shell that came back at every `exit` could not be
    left, and tmux users expect the last `exit` to end something.
  - A keys directory written by an earlier build has no `.empty`: the
    viewer then shows the sentence as before.
- **D86. The fix check of D85 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  Opus found no must-fix; astra three. Changed: "no pane" is what the
  layout says, not what there was room to draw (a terminal of no rows
  started a second shell in a workspace that had one); two viewers that
  find a workspace empty in the same moment start one shell (`.empty`
  claims with a folder `.first`, which the other finds; the claim goes
  when the script ends); a script that is being waited for when `.empty`
  is started is still waited for; the sentence names `C-b t n` only if
  that key is there. The test types through the viewer and counts the
  runs of an `.empty` of its own.
  - CI: the sentence was longer than the test's screen on a machine with
    a long temporary path (it is short now, without the path).
  - Kept: a layout made invalid in the instant between the viewer's two
    reads of it can still get `.empty` run against it.
  - From the tester: a `SHELL` that cannot be run left a blank screen and
    no word (the pane removed itself with the shell's error on it): a
    default shell pane whose program could not be run at all now waits
    for Enter. "First shown" is each time a viewer comes to a workspace
    and finds it empty, also one whose last pane ended while the viewer
    was elsewhere; the README says "each time". Kept: an `.empty` that
    fails shows its error in the corner and not the sentence; a slow one
    is waited for two seconds, as any key script.
  - Found later by the test failing 2 runs in 36: a default shell pane's
    `rm -rf` of its own folder could find it "not empty" (the holder wrote
    a file in that moment), and the pane stayed, dead. `.shell` removes
    again until the folder is gone (a second at most).
- **D87. The fix check of D84 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  The tester found the two files line for line the same through real
  programs, resizes and rotation; the reviewers traced where they could
  part. Changed, by one rule each:
  - what waits to be written is counted on the longer of the two texts,
    for every way rows reach history (a resize bypassed the bound; the
    holder counted the plain text only); a row that is refused ends the
    open line in both texts;
  - when anything fails on either file, the log says so, `history` keeps
    what it has and `history.styled` is begun anew: a reader never pages
    colour that belongs to other lines. Not "check both first": a full
    disk cannot be checked beforehand;
  - neither file is put aside while its last rows are a resize's to take
    back (they were moved out of reach and then showed twice; the plain
    file had this before);
  - the U+200B that keeps two characters apart is decided across a row
    join too, also before the copy of a plain row;
  - `.text` drops the rows the plain screen says are empty, in both modes.
  - Cost: the flood 1.51 s to 1.56 s. A single write or resize that makes
    more than 4 MiB of styled text now loses plain rows it kept before
    (colour at every cell of a full 500 by 500 screen): the README's
    Bounds say so. A file is 4 MiB and one write, about 4.5 MiB as a rule.
- **D89. A pane owns its colours** (message 23, step 5; the panel 7 of 8).
  What a program sets (OSC 4, 10, 11, 12) is kept by its pane's emulator,
  one set for both screens, until it gives it back (OSC 104, 110 to 112,
  a full reset). A question is answered with what was set, else from the
  session's `colours` as before. The cells stay as the program wrote them
  (a palette number, a default); `screen.styled` tells a cell whose number
  or default was set as direct colour, so text printed before a set
  changes with it, as on a terminal. With a background of its own every
  cell of the pane has it, blank ones too. The cursor's colour is a fifth
  word of `cursor`; the viewer gives it to its terminal for the pane that
  has the keys and takes it back when the keys move, a menu opens or it
  leaves. The viewer never sets its terminal's palette. D56's "heard and
  changes nothing" is gone.
  - No `palette` file: the cells carry the colours.
  - `history.styled` is a record: a row is told in the colours of the
    moment it left the screen, and is not rewritten when a colour is set
    or given back later.
  - Taken: `#` with 3, 6, 9 or 12 hex digits and `rgb:R/G/B` with one to
    four each. Not taken (a line in `log`): colour names, `rgbi:` and the
    other X forms.
  - The viewer also asks its terminal for the cursor's colour at start and
    keeps it in `colours` (a line `cursor`), so "give back" has something
    to go back to.
- **D90. The command being typed is not wrapped anew** (message 23, step
  6; the panel 8 of 8 in its second round). This reverses D69's "not
  fixed": there bash's doubled command after a narrowing was left as
  bash's own, since not wrapping the cursor's line would lose text. The
  shell's own mark removes the objection: where the last OSC 133 mark is
  a prompt's or a command's (a command is being typed, nothing of it has
  run), the rows of that line are cut or padded at a resize instead of
  wrapped, so the shell's redraw, which counts rows up from the cursor as
  it left them, lands on them clean. What is cut is text the shell is
  about to draw again; finished output is above the prompt and is wrapped
  as ever. Without marks nothing changes, and with the command's output
  begun (mark `output`) nothing changes either.
  - Tried with a real bash at 30 columns made 17 and back: one command
    line with marks, the doubled one without.
- **D91. The fix check of D90 (1 Opus, 1 gpt-6.2-astra, a tester; one pass).**
  All three showed finished output cut and lost: D90 kept the whole line
  the cursor was on whenever the last mark was a prompt's or a command's.
  So output that did not end its line (the prompt after it on the same
  line), a shell that marks only its prompts, and a running command of a
  shell that sends no mark for output all lost text at a narrowing. The
  rule now asks for proof that the line is the shell's own: the last two
  marks are a prompt and a command's start, in that order and on the
  screen, the command's after the prompt's; the cursor is at or after the
  command's start and on its line; and the prompt began that line or an
  earlier one. Anything else is wrapped anew, whole. The unit test holds
  eight such cases to "the text before is the text after".
  - The rows kept stay one line (they lost their mark of going on, so a
    second narrowing before the shell had drawn doubled the command
    again, and `wraps` and history took them for separate lines). A change
    of height alone does nothing of this.
  - Kept: text a program prints on the prompt's own line after the
    command's mark is taken for typed text (a shell moves to the next
    line before a command prints); a mark beyond the cut is at the cut
    until the shell draws its prompt again; a shell that does not redraw
    after a resize leaves its command cut.
- **D92. A picture that does not fit its tile is drawn: the part inside**
  (message 23, step 7; the owner: no placeholder, done properly). The
  holder still passes a picture through as a file. The viewer, where a
  picture does not fit, reads the sixel into pixels (`src/sixel.rs`,
  which names nothing else of the program and compiles alone), cuts the
  rectangle its visible cells cover and writes that as a sixel of its
  own; a picture that fits is passed on unread, as before. So a wider
  tile shows it whole again.
  - A picture stays while a row of it is on the screen: off the top it
    goes row by row, and `pictures` then has a ROW below 0 (a reader of
    unsigned numbers skips the line, which is what happened before). A
    resize keeps a picture that is now wider than the screen.
  - Bounds: 4 MiB read and 4 MiB written; a part of at most its cells
    times the cell's pixels and 2^23 pixels; registers 0 to 255; work
    counted in pixels painted, not on a clock, so a picture is refused
    or drawn the same on every machine. A refusal is said once in the
    viewer's corner (the pane's `log` has one writer, its holder).
  - A part is written with square pixels and unset pixels left unset
    (`P2=1`), so no terminal fills the last band's unused rows over the
    line below the tile. No picture is drawn on the terminal's last row.
  - A picture's height is its lowest painted pixel, not its last band of
    six (it took a row too many).
- **D93. The fix check of D89 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  Changed: one OSC 4 may have any number of pairs (its loop had a counter
  that overflowed at 65,280); the bound on answers no longer drops the
  sets that follow; a colour form that is refused, and a palette number
  that is not 0 to 255, each have their line in the log (a question about
  such a number stays silent), which the test now really asserts.
  - The cell a wide character leaves empty at a row's end is told in
    `screen.styled` when it has a look (a colour): as U+200B and a blank
    at the row's end. A painted blank alone could not be told from one
    the program printed, and a reader joining rows by `wraps` would gain
    a blank; U+200B is fux's own mark in the styled files already.
    `screen`, `wraps` and both histories leave the cell out as before;
    `keys/.text` takes the pair out where it joins rows.
  - The cursor's colour: taken only for a pane whose program lives; given
    back (OSC 112) in the string that restores the terminal, on every way
    the viewer ends that it can act on. "Its own" is the colour the
    terminal is set up with: a colour another program had given the
    cursor before the viewer started is not put back, as its shape is
    not.
- **D94. Search and restore are scripts** (message 23; the panel split
  between "now" and "recipe only", and no seat wanted a subcommand).
  `C-b f s` (`keys/.search`) greps every pane's text and opens the line
  chosen with the helpers the other find keys use. `keys/.restore` starts
  every ended pane again; that a live one is left alone is `fux start`'s
  own refusal of a name a holder keeps, so the script needs no test of
  liveness (the pids in `status` mean nothing after a reboot).
  - No new file. A file `argv` (the arguments in the shell's own quoting,
    written once; before that, zero bytes between them, which macOS's
    xargs could not read back) was added for this and taken out again by
    the owner's word (message 26: "argv seems spurious"; D96). The script
    starts the words of `status`'s `command` line, split at blanks, with
    nothing expanded and no shell run over them. That line is the command
    flattened for a person to read, so `sh -c "a b" x` cannot be split
    back out of it: the recipe is honest rather than exact, and says so
    (its comment, the README): an argument with a blank or a quote starts
    wrongly or not at all, the default shell panes and plain arguments
    start as they were, and a project that needs exactness keeps its own
    start script. Given up: restoring every pane exactly. Kept: one fact
    fewer to write, read and explain. No restart command, no saved
    environment.
  - A start in place begins a pane's files anew, so the script keeps what
    the pane had printed in `before` first. Keeping `history` across a
    restart in the holder was a seat's wish; the script does it without.
- **D95. The fix check of D92 (1 Opus, 1 gpt-6.2-sol, a tester; one pass).**
  The tester's pixel checks found every cut part exact; the reviewers
  traced faults around the cut. Changed:
  - one measure, the viewer's pixels: the viewer reads each picture's
    real size and holds it to the tile's room in its own terminal's
    cells. The pane's count of a picture's cells is no longer used for
    drawing (a holder told another cell size could say "it fits" of a
    picture that passed the line between tiles). A picture is passed on
    unread only if it is square of pixel, has nothing above the tile and
    fits; anything else is cut. A terminal that does not say how many
    pixels a cell has gets no pictures, with a line in the corner
    (before, one that "fit" by cells was passed on): without a cell's
    size nothing shows that a picture stays in its tile;
  - the budget counts what is painted inside the part, 16 times a pixel
    on average, and the colours used inside it (a picture cut to its
    last rows, or a photo in a short tile, was refused for what lay
    outside);
  - a picture file replaced under its name is a change; a picture
    clipped at the bottom is kept by the resize that reveals it; one
    beside no text keeps its column through a narrowing; an open menu
    hides only the pictures it covers; a frame's pictures are 16 MiB
    together at most.
  - Refusals stay in the viewer's corner, every one of a frame joined,
    set before the corner is drawn, shown to the next key. Not the
    pane's `log`: it has one writer, and a refusal depends on the
    viewer's tile, which the pane does not know. This departs from the
    order's "a log line", for that reason.
  - Kept: text written over part of a picture removes the whole picture
    (the README says so); a cut picture has unset pixels left unset
    whatever its own header asked.
- **D96. A pane's files: no `argv`, `screen.main`, and `lines` for `top`
  and `wraps`** (the owner, messages 24, 26 and 27). Asked what else
  could be a file of its own, the owner took four ("all good ideas"),
  then three back ("argv seems spurious..so does colours maybe, and idk
  about foreground either? screen.main seems very useful tho"), then
  said "merge top and wraps".
  - No `argv`: see D94. Restore reads `status` and is honest, not exact.
  - `screen.main` and `screen.main.styled`: the main screen under a
    full-screen program, as the screens would have it, there only while
    `modes` says `alt` (written before it says so, removed before it
    stops), written again only when it changes (a resize, a colour of
    the pane's own). The holder renders the covered screen at each
    publish to compare it; no flag says "it changed", which would be a
    second truth to keep. Given up: nothing a reader had.
  - `lines` in place of `top` and `wraps`: its first line the number of
    the screen's first line, its second the rows that go on. Nothing
    read one without the other. Checking what they meant under a
    full-screen program found them of two screens: `top` (and `marks`)
    of the main one, `wraps` (and `links`) of the one shown. That
    stays, and a third line, there only then, says which rows of
    `screen.main` go on: without it the covered screen's wrapped lines
    and the marks in them could not be joined. Given up: the old two
    names (no shim: pre-release), and `cat top` as one bare number
    (`read` and `sed -n 1p` take the first line).
  - Not added: a pane's `colours` file and `foreground` (message 26).
- **D97. Three crates: fux-vt, fuxix and fux** (the owner's word; the form
  by a panel of eight, agreed in one round). D42 kept one crate and a
  script. The owner asked for the split; the panel settled what goes
  where.
  - **fux-vt** 0.18.0: the emulator (`term.rs` is its `lib.rs`), the
    widths, cells and styles, the row writers, and the sixel scanner and
    cutter (8 of 8 for sixel there), with their unit tests. **fuxix**
    0.11.0: every `unsafe`. **fux** 0.40.0, the root package: holder,
    viewer, layout, the command line, the key scripts and `keys.rs` (5
    seats of 8: its names are the scripts' file names, and it reads the
    outer terminal). The versions are above what was published under
    those names (0.17 and 0.10); all three are `publish = false` until
    the owner says otherwise.
  - The move changed no code: the program still says `term`, `sixel`,
    `width` and `sys`, now names for the two crates (`main.rs`). The unit
    tests went with the code they call (27 and 8, the same 35); the tests
    of what the emulator and the key names agree on stay with the keys.
  - **`boundary.sh` is gone.** It compiled the emulator, the system
    calls, the key names and the sixel cutter each alone, so that none
    could name the rest of the one crate (D42: "what keeps a later split
    cheap"). The split is made: the crate graph says who may name whom,
    and `#![forbid(unsafe_code)]` in fux and fux-vt says where `unsafe`
    may be, which no module can allow away as it could the manifest's
    `deny`. CI holds `Cargo.lock` to exactly the three names.
    `sizes.sh` counts each crate and their sum.
  - **fuxix's public surface is safe to call and owns what could be
    forgotten**: one open that does not wait (no caller writes
    `O_NONBLOCK`), a wait on borrowed descriptors with one answer for
    each, `openpty` declared with the types it takes, the number of "no
    reader on a FIFO", and raw mode as a value that writes the terminal
    its leave-taking and puts the mode back when dropped.
  - Given up: a negative count from the split itself. It cost 6 lines
    (two crate roots' headers); the safe owners took 1 back. The panel
    said so before the work: the lines come from the simplifications.
  - Not done here: `Term`'s fields private behind a `Screen` and one
    `Out` (8 of 8, a change of its own: D98).
- **D98. The emulator's interface: a `Screen` read, an `Out` taken, a
  `Told` said** (8 of 8 in the same panel). No field of `Term` is public.
  - **`Screen`** is what is shown: the size, the grid (`cell_at`), the
    cursor, the modes and the pictures on it. The holder reads its
    terminal's by reference (`Term::screen`). The viewer holds one for
    each pane and no emulator: `Screen::read` makes it of
    `screen.styled`, beside the writer of that file in fux-vt, and the
    round-trip unit test reads back with it.
  - **`Out`** is everything produced since it was last taken, as before.
    **`Told`** is what comes from outside (the name for XTVERSION, the
    keyboard flags the terminal takes, a cell's size, the session's
    colours), reached through the one method `Term::told`.
  - What can no longer be written: a holder or a viewer setting the
    cursor, a mode or the size of an emulator; a caller that drops a
    picture from one screen's list and not the other's (`has_picture`,
    `keep_pictures`); a viewer feeding an emulator cursor moves to draw
    a file. A mode that is one switch is kept in one place (`switch`)
    for set, reset and the question: none can be set and then answered
    "not recognised".
  - **The cursor is one `Cursor`** (row, column, style, origin, the
    character sets, whether it waits to wrap), which a `Screen` has and
    DECSC saves by assignment: `Saved` is gone, and nothing can be added
    to the cursor and not saved. A soft reset keeps its place and takes
    the rest as it starts.
  - It cost lines: 7,127 to 7,149 (fux-vt +44, fux -22). The private
    fields and the reader's move were +49 (the two structs' headers, the
    methods that took the fields' place, and the lines rustfmt wraps
    for `self.screen.`), where the panel had estimated -40 to -100
    without building it; the switch, the cursor and what they let be
    tidied gave 27 back. The target of 7,100 is not met by this series.
  - Left out, because each adds lines or changes what a pane publishes:
    - the wrap and gap marks in the row and not in a cell's style: a
      mark now travels with its cell (ICH at a wrapped row's first
      column moves it away, DCH brings it back, and `lines` says so),
      which a flag on the row cannot say byte for byte;
    - marks as a struct with an enum of kinds, and a resize's places as
      a struct: about 15 lines more, for kinds that one `match` makes
      and places that `reflow` takes as one slice.
  - Still the viewer's: the words of `modes`, the `cursor` line and the
    records of `links` are put on its own `Screen` by `view.rs`
    (`set_words`, `set_link`, the cursor's fields).
- **D99. The simplifying run changes no behaviour: what its fix checks
  found was put back** (message 29's rule). Two readers for each series
  asked only "did behaviour change". Put back as it was at `525e2c8b9`:
  - the answer to `DECRQSS m` lists the looks as 1, 4, 5, 7, 8, 2, 3, 9
    (one table of looks had made it 1, 2, 3, ...; a test holds it now);
  - `layout` and a key script's caption are opened by a plain open, which
    waits on a FIFO, and a picture whose read ends in an error is not
    drawn (one bounded reader had made all three "what could be read");
  - only a screen's stamp has its length; a notice's, a picture's and
    the clipboard's are inode and time (one stamp had given all a length);
  - a frame numbers a link's address when a cell with it is drawn, and an
    address once (laying every tile's list end to end ran out of the
    65,535 numbers with 66 tiles of a thousand addresses each);
  - a wait passes the system only the files up to the last one that is
    there (four entries with two absent failed on Linux under a limit of
    three open files, where the old two entries did not).
  - a pane the viewer has not read yet is one blank cell with the cursor
    shown, as the emulator it used to hold was (an empty `Screen` had
    hidden the cursor of a focused pane with no `screen.styled` yet);
  - Kept, and so the two changes of this run: a split or new-tab script
    reads `keys/.new-shell`, which is written with them, as `.shell` is
    (a keys folder put together by hand needs both files); and a viewer
    whose terminal cannot be duplicated at its start now leaves the
    terminal's mode alone (it was left raw: raw mode is a value that owns
    the duplicate, D97, so the duplicate is made first).
  - The last series (what was dead or said twice, 32 commits) was read
    the same way though the message asked it only of the first three:
    no change found. (Two things no user can see: `fux layout` reads
    `FUXN_DIR` before `FUXN_WS`, and a resize after a hang-up is refused
    one step later.)
  - **Speed, measured at the end** (`tools/perf.py`, both builds back to
    back). The flood is 3% slower: 1.58 s where it was 1.53 s. It came
    with "History's tail and the rows a resize lent are two types"
    (`e25d9653d`, found by building each commit). One cause is put right:
    every scroll asked where the marks are, also with no mark (a scroll
    now asks only if there is one). The emulator alone is then 1.45 s for
    the two million lines where it was 1.42 s; the rest did not answer to
    four trials (inlining, the drop of the lent rows) and is not found.
    The key rows land on 2, 3, 4, 5 or 6 ms from run to run with one
    build, so they say nothing at this size; the others are inside
    their spread.
- **D100. The least code that is still the whole multiplexer: guards and
  second copies go** (the owner, 2026-11-20: "no guards against the user
  or the filesystem", "negative LOC", "behavior changes are allowed if its
  a simplification"; and 2026-11-21: "single source of truth should be a
  principle"). An audit by six readers, one area each, and a second pass
  by two for facts held twice; the writer alone decided. Where an earlier
  entry had added a guard after a reviewer's finding, this entry takes it
  back and says what Unix does instead; owner's decisions and panels'
  (no copy mode, no zoom, two session files, `FUXN_ROW`/`FUXN_COL`, the
  rows a resize sends to history coming back) are not reopened.
  - **D23, in part.** `fux start` takes no lock on the workspace: `mkdir`
    claims a new name and the pane's own lock says who starts it. A start
    that failed removes the folder it made under that lock (the old
    removal, under the workspace's, could take another start's folder).
    The start is done when the holder says so on a pipe, not when a loop
    finds `status` naming it.
  - **D27, D84/D87, in part.** What cannot be written to `history` or to
    `history.styled` is lost to that file, and `log` says so.
    `history.styled` is not begun anew, tracked as lost and tried again.
    The two files are put aside where the last line of what is being
    added ends, so nothing remembers, or reads back from the file,
    whether it ends inside a line.
  - The last screen is written once: no two seconds of trying.
  - **D86, in part.** `.empty` claims nothing: two viewers that find a
    workspace empty in the same instant start a shell each. (A script
    killed after its claim left `.first` behind for good.)
  - **D71, in part.** `.shell` does not settle the layout at its end:
    every edit and every viewer does.
  - A notice that appears after the viewer first looked at a workspace
    is new: no comparison of file times with the clock. A pane moved
    there with an old notice is rung once.
  - A holder makes its FIFOs anew; it does not look for ones to keep.
  - `fux layout pane NAME` names a pane, `focus` takes a way: one way.
  - The names fux will not make are one list (`layout`, `layout.new`,
    `colours`, `keyboard`, `-`), in exact case.
  - `fux wait` enters the directory by name with std; `fchdir` is gone.
  - **D99, in part** (its "no behaviour change" was that run's rule, not
    a decision about the things): one stamp (number, time, length) for
    every file; capability answers bounded by the one bound on answers.
  - **One source for each fact.** Gone as second copies: the holder's
    list of what the program said last (beside what waits to be written
    and what each file holds), its word for running or hung up (beside
    the pty it holds or not), its count of 2026s (beside the
    emulator's), its `status` text (beside the file); the viewer's
    counter shared with its writer thread (beside the writer's own word
    on the pipe), its keys kept as "nowhere" (beside not kept), its
    second stamp; the emulator's count of lines gone into history
    (beside where history ends), `main_row` (beside the saved cursor),
    `string_over` (beside the string's length); `contains` beside
    `panes`; `read_some` beside `text`; the split's tree operation
    written out for the edit and run again, through the edit's words, for
    the viewer's prediction.
  - **Types.** As the last run found for its two, a type that adds lines
    and removes no check is left out: a pane's state as an enum, `Way`
    for a split's bool, rows and columns as newtypes, a validated `Name`
    (two audits, each showing the lines). What a type now holds: a key
    with nowhere to go cannot be in the list of keys down; a workspace's
    name that is none cannot become `main`; the way to the terminal's
    writer is owned and dropped, not an `Option` each write looks into.
  - **Bugs found on the way**, each with a test that failed first: an
    editor that ends while its key's script runs (the macOS failure of
    the last run); Alt with a character split across two reads; the bar
    on a terminal over 250 columns; `FUXN_WS` that is no name taken for
    `main`; `ESC # 8` leaving pictures; a blank joined to a Prepend
    character lost from `screen`; a sixel register over 255 used only
    outside the part drawn refusing the part; a hostname of 255 bytes
    cut by one (no test: it needs such a machine).
  - **Tests changed on purpose**: the unwritable history file (the new
    rule); the alternate screen entered by 1047 at a resize (blank rows
    kept down to the saved cursor's row, as by 1049); forty capability
    names in one question (forty answers); the picture that cannot be
    cut (it painted its register 256 only outside the part).
- **D101. The second wave** (the owner, 2026-11-21: "keep cutting code
  while making bugs impossible and preserving features"; three readers on
  the tree after D100, and two for facts held twice). Taken:
  - **D100's first point, changed.** A start that failed removes
    nothing: the folder it made stays, with a `log` that says why (three
    readers: the removal named the folder by its path again, which may be
    another's by then). `fux start` makes folders and removes none.
  - A viewer shows the workspace it was told to show, as its folder is:
    removed, it says so and stays. (It moved by itself to the workspace
    shown before, or to `main`.)
  - `FUXN_SELF` is gone: the find key offers every workspace. Only that
    key left the viewer's own out; three other ways to show it did not.
  - The viewer writes `colours` and `keyboard` once, when its terminal has
    answered the last question or after ten seconds; no list of what it
    wrote last.
  - A round of writing that failed is tried again when the pane's
    activity brings the next round, not by a timer every 25 ms; `log`
    says so for each round that failed.
  - **One source.** That a program has the alternate screen is said by
    `screen.main` being there: the word `alt` is gone from `modes`. The
    title is the emulator's (`Term::title`), a fact as the others; an
    empty one is no file. History waiting to be written has one copy, the
    emulator's, taken when it is written (the bound of 4 MiB counts what
    still waits from before). A notice's rule (no control characters,
    1,024 bytes) is the emulator's for `notify` on `ctl` too; its number
    grows with each round of writing that had one. The two marked tabs of
    a layout are followed by their places, not found again by their
    panes' names. A key script's pane is started in the workspace
    `fux start` itself works out. Each FIFO a holder or a viewer owns is
    one descriptor, open to read and to write.
  - A split's parts end where their shares end (one pass; the cells of
    equal parts may fall 2,1,2,1 where they fell 2,2,1,1).
  - **Bugs**, each with a test that failed first where one could be made:
    a name with its workspace refused for the starter's own workspace's
    name; a session named by a relative path told so to the pane's
    program; the last read of the pty passing its 4 MiB.
  - **Left out, and why**: `Csi.bare` and `huge` (the kitty protocol's
    own words for `CSI < u` and a number too large); `string_holed` (D68);
    the filter in `Screen::read` (without it a file someone wrote can
    stall the viewer); telling the emulator its lent rows stay when
    history is put aside (`lent()`: a test of D87 says they come back);
    links inside `screen.styled` (D67: the screens stay text); the
    `lost` label and the lock looked at for it each frame (the look, and
    D4); the hint that names `C-b t n` (D86); the default keys that do
    one thing under two names (`b`/`n` and `t h`/`t l`, `m h`/`m k`, `m
    j`/`m l`: fux 1's keys). **For the owner**: whether a resize may drop
    the main screen's pictures (about 24 lines of following them through
    the wrap), and whether the constants of `fuxix` should say which
    Linux machines they are for.
- **D102. The third wave** (three readers on the tree after D101). Taken:
  - **D69 and D70 on mode 2026, replaced.** Nothing a program wrote
    waits more than half a second to be written, drawing or not: the
    half second counts from the oldest output not yet written, not from
    the first 2026 with no end since. The old rule froze a pane whose
    program drew frame after frame with no pause (each new 2026 seen
    between two looks started the half second again; a test shows it).
    The holder's `drawing` and the emulator's count of 2026s are gone.
  - What a program said once (a picture, the clipboard, its directory,
    a notice) stays in the emulator until the round of writing that
    takes it, and is written in that round or lost with it, as history
    is; the holder's copy of it is gone. A picture's bytes are its
    placement's until then: erased before a round, they are gone with it
    (they used to wait in a list with no bound).
  - A holder started again in place removes no picture files of the
    holder before it; `pictures` names the live ones.
  - A `COLUMNS` or `LINES` that is set and is no size fails the start,
    which says so: no size is made up.
  - `.text` takes one reading of a pane (it read again, five times at
    most, if the pane had printed meanwhile): of a pane that is printing,
    a line may be there twice and a mark's line off by what scrolled.
  - **One source.** One parser for the kitty forms of a key (`event`
    read again what `sequence` had split). One look at each tile's pane
    a frame. One operation owns the link the terminal has open. A resize
    takes back what history had of the rows above before it gives them
    again (they were counted twice against the bound). Whether a line
    feed is the text wrapping is told down the call, not kept in the
    terminal. The names fux will not make are read out from their list.
  - **Types.** A pane with no program cannot own a key's release
    (`NonZeroU32`); what became of the program is given to the round that
    writes it, not kept beside the pty.
  - **Bugs**, each with a test that failed first: the frozen pane above;
    two programs that ask for the mouse's presses alone (a click that
    moved the focus swallowed every later one); a FIFO named as a key
    hanging the viewer; a resize joining a mark to a character the cursor
    had left; a painted blank lost when the screen is wrapped anew;
    erasing in a line leaving the marks of what it erased on what is
    written next (`C-b C` would have opened the wrong text); a control
    character of two bytes in a hand-written `screen.styled` dropping
    the rest of its row; a second resize before a round refusing rows
    early; erased pictures waiting without bound.
  - **Left out**: the holder's look every two seconds for its `in`
    (removing a pane's directory must end its pane: the README's first
    "don't"); the places of a resize given to `reflow` by reference
    (risky for 6 to 10 lines); the frame's budget for pictures
    remembering why it refused (rare, and more lines); `.search` opening
    by a line's number after history was put aside meanwhile.
- **D103. The fourth wave** (three readers, each with one question:
  what a type would hold, what a script could do, what says the same in
  fewer lines). Taken:
  - **D36, in part.** The viewer keeps the workspace it shows and
    nothing else of workspaces: its memory of the one shown before, and
    `workspace -`, are gone (no key that comes with fux used it, and
    since D101 a viewer goes nowhere by itself). `workspace NAME` shows
    that name; with no folder of it, the viewer says so.
  - `Term::resize` takes the nearest size a terminal may have, as
    `Term::new` does, and refuses none: the holder refuses a size on
    `ctl` in its own words before it asks.
  - **One source.** Whether bytes are a character is asked of std (the
    emulator and the viewer's key reader each had a table of lengths and
    the emulator its own check of the shortest form). A workspace's
    first frame is the one before its notices were looked at (`greeted`
    is gone). Whether the terminal joins characters, and by whose doing,
    is one value. A key's script is told of the layout its key was read
    against. What the terminal told is taken apart once.
  - **Bugs**, each with a test that failed first where one could be made:
    a FIFO named `layout` freezing the viewer; a clipboard set while a
    workspace with no pane was shown sent on the way back; `.restore`
    under a relative `FUXN_DIR` starting panes in another session;
    `CSI @` and `CSI P` leaving a shell's marks behind their text;
    `.text` opening `screen` twice (no test: it needs the file replaced
    between two opens of one awk).
  - **Left out**: REP repeating by the rule a mark joins by (the
    referee's `rep-after-cancel` would differ from xterm: `A` where
    xterm has `A`, `AAA` after); the keys installed outside `fux view`
    (the quick start is one build and `fux view`); a layout settled only
    in memory and written on edits alone (D71); `fux wait` saying
    nothing (its two lines are the agents' page's recipes); one try for
    the pane's lock in `fux start` (a viewer's own look would fail a
    start); the cursor's waiting wrap kept in its column (fifteen places
    for five, on the print path); a key's kinds as an enum (written out,
    3 to 9 lines more).
- **D104. The fifth wave** (four readers: the docs against the code, the
  small files read least, the key scripts, and the whole once more, who
  found "the tree is close to its minimum"). Taken:
  - **D46, D100, in part.** Whether a pane's holder lives is, to the
    viewer, whether anything reads its `in`: the viewer takes no lock.
    `fux start` therefore takes the pane's lock in one try (it tried
    twenty times, 5 ms apart, for a viewer's look to pass). The lock is
    still the proof (D4) for `fux start`, the holder and `fux wait`.
  - **D71, in part.** `FUXN_EDIT` is a line of sh run with the file and
    the line as its arguments (`hx "$1:$2"`), not words in which `%f` and
    `%l` are replaced: a setting that is a command is run by sh, as a
    picker's command is, and nothing is spliced into text.
  - `.new-shell` is a script that is run, as the other helpers are, not
    a file to read for a function. `C-b w n` makes the folder and shows
    it: the first shell there is `.empty`'s, as in any workspace with no
    pane.
  - A capital is `S-` and its small letter in both forms a terminal
    tells keys in, in any alphabet (`É` was the key `É` in the old form
    and `S-é` in the new).
  - The docs were read against the code, sentence by sentence, and
    corrected where they said more than the code does (what `fux wait`
    proves, what the agents' recipes establish, what a round of writing
    is) or said a thing in two places.
  - **One source.** How many columns a character adds is in the table
    the tool writes (the rule was applied again at every look). The
    keypad's keys are named by one list.
  - **Bugs**, each with a test that failed first where one could be made:
    a click whose release was never told of taking the next click;
    `width` finding U+FE0F past a character's end; a shell that ended
    after a mistyped command said to be a program that could not be run;
    a picker writing its choice to a viewer that had gone, as a file.
  - **Left out**: one session file in place of `colours` and `keyboard`
    (D55, D57: for the owner, with the case in the fifth audit); `C-b a
    k` hanging up through `ctl` (the key sends TERM, as fux 1's does);
    the picker taking a number only (the README: a number or a name);
    the viewer's own words for typing into a pane that has ended.
- **D105. The sixth wave** (three readers; each found its part "close to
  its minimum"). Taken:
  - The holder works out what is due next once: the round of writing,
    the refill of what it may read, and the look for its `in` are each a
    time left from one reading of the clock, and the wait is the least
    of them. (Two sets of conditions said when work was due and how long
    to sleep; they disagreed by up to a refill's wait under mode 2026.)
  - A pane is up when its holder said so on the pipe, and only then
    (D100's "gone with its holder, it started, ran and ended" is taken
    back: a folder renamed meanwhile was told as a success). A program
    that removes its pane's folder before the first screen fails its
    start.
  - One function reads a FIFO's bytes into bounded lines, for the
    holder's `ctl` and the viewer's; the bound is counted in the bytes
    written. The session's whole path is worked out once.
  - **Bugs**, each with a test that failed first where one could be made:
    a run of ESC bytes making the key reader call itself once a byte (a
    stack overflow at two thousand); a click read against the tiles of
    the workspace shown before; text with no key (the kitty form's key
    0) given a release of its own; a FIFO where a named pane's folder
    should be blocking `fux start`; rounds of writing that fail leaving
    their pictures' files for good; characters put in or taken out at
    the start of a row that goes on cutting the row from its line;
    `ESC # 8` keeping the marks of what it wrote over.
  - **Tried and taken back**: marks carried through a resize as places,
    as the cursor is (it left marks on a screen cleared after the rows
    came back; keeping the old rule cost what the change saved); the
    width of a character asked once in `print` (8% slower in a flood);
    a frame drawn from `screen.styled` alone (a test replaces a
    picture's file with no word from its pane and waits for it).
  - **Left out**: the modes that are one switch in one table (the order
    of the words in `modes` is the file's form); one writer for a
    style's codes (the bytes of `screen.styled`); a frame's links as its
    tiles' lists end to end (D99); the cursor drawn where its file says
    without keeping it inside the rows read (the design's promise).
- **D106. Six cuts that had been set aside for the owner, settled by a
  panel of ten** (2026-11-21; 2 Opus, 4 gpt-6.2-sol, 4 gpt-6.2-astra,
  each with one point of view: a daily user, the maintainer, a writer
  of scripts and agents, a Unix purist, a terminal-standards reader, a
  newcomer, someone coming from tmux and fux 1, a reliability reader, a
  reviewer counting lines against loss, a user of pictures and colours;
  seven of ten settles; the ballots are in the run's notes). The owner
  may overrule any.
  1. A resize keeps following pictures: **kept, 10 of 10** (a picture a
     command printed once and left is never drawn again; splitting the
     pane would lose it).
  2. One session file in place of `colours` and `keyboard`: **left as
     two, 8 of 10** (Opus 0/2 for two, sol 4/4, astra 4/4): one write
     sets the keyboard flags and leaves the colours as they are.
     Dissent: one writer, one moment, one reader.
  3. `C-b a k` hangs up the pane's program through `ctl` instead of
     sending TERM to a pid read from `status`: **taken, 9 of 10** (Opus
     2/2, sol 4/4, astra 3/4). Dissent: a program that ignores a hangup
     and ends cleanly on TERM has no key of its own now.
  4. One default key for each thing (`C-b t h`, `t l`, `m j`, `m k`
     go): **taken, 7 of 10** (Opus 2/2, sol 3/4, astra 2/4). Dissent:
     the move menu no longer takes all of `h j k l`, as focus and
     resize do.
  5. `fux wait` only waits and prints nothing: **left as it is, 8 of
     10** (it reads the pid and the state under the lock of the
     directory it waited on; a path read afterwards may be another
     run's). Dissent: it restates two files.
  6. The words of `modes` in one table's order, and one writer for a
     style's codes in xterm's order: **taken, 10 of 10** (the order of
     bytes in two files changes, their meaning does not).
- **D107. The seventh wave: a hunt for bugs by reading** (three readers,
  one each for the emulator, the holder and commands, the viewer and
  layout; each finding a traced input). Fixed, each with a test that
  failed first: text inserted in insert mode kept the marks where they
  were; a screen cleared with `CSI H CSI J` (what `clear` sends) or
  `CSI 1 J` from its last cell is a cleared screen, as with `CSI 2 J`,
  and a resize gives it no rows back; a mark scrolled off the bottom of
  a scroll region is at the end of its last row (it had a column no
  other path understood, and `CSI @` there overflowed); a workspace
  name in `FUXN_WS` that is no text is refused (it meant `main`);
  `size -N` never grows a share; a history file that cannot be put
  aside begins anew (it erased the other's archive each round);
  `fux start` enters the pane's folder before it locks it (a folder
  moved between the two was held under the wrong name); keys typed
  after the key that leaves are not typed into the pane; half a
  character waits for its rest however late that comes. Left, listed in
  the PR: the findings whose fix costs more lines than the case is
  worth (a history file that cannot be appended to, `FUXN_PANE` after a
  move during a start, `fux layout` resolving its path twice).
- **D108. The eighth wave** (three readers, one each for the emulator,
  the holder and commands, the viewer with layout and keys; each said
  its part is near its least and named a handful of lines). Taken, the
  bugs each with a test that failed first: half a wide character
  erased is the character erased, for marks and pictures too; text
  that wraps on the last row below the scroll region joins no other
  line; a sequence with an intermediate byte too many is none of the
  ones known (`ESC # # 8` filled the screen; the character sets of
  `ESC (` and `ESC )` aside); a styled row longer than
  500 columns is cut as at any width; a pane started again that never
  came up, once it had written its own `status`, leaves no `state` of
  the run before (`fux wait` printed the
  new pid with the old end); the old `cwd` is removed before the
  program runs (a program that wrote its own at once lost it); a file
  is read again when its inode's change time moves (a `colours` that
  could not be read stayed unread for good; a chmod alone now costs
  one more read); the window's focus is told only to a program that
  still asks; a picker's number `0` is a name, with no word from sed;
  the bar leaves out whole a wide character it has no room for. Cuts:
  a control string is matched whole, once, and one function says what
  a sixel picture is, for the emulator and the viewer; marks are moved
  once per erase; the pty's ends are closed-on-exec by std (one unsafe
  block fewer); one place charges a read to its budget; the program's
  words are passed on as given; no field for the pty's slave; lines
  after `quit` in one read are not done. Left: tab stops and saved
  slots as arrays (every line saved is an index the lints refuse); a
  drag that goes on after another viewer moved the focus goes to the
  newly focused pane (6 to 8 lines to pin it); a SIGINT blocked by
  whoever ran `fux start` stays blocked in the pane (4 lines of FFI;
  the README's "Don't" list says so); a FIFO put where a
  workspace's folder is makes `fux layout` wait (not guarded: D100).
- **D109. The ninth wave: which whole concept can go** (two readers of
  the whole tree). Taken: `fux layout tab N` (a tab by its number: a
  tab is shown by a pane in it, `pane NAME`; `tab next|prev|-` stay);
  the holder takes from its program's environment the four names a
  key script is told of its viewer (`FUXN_WS`, `FUXN_VIEW`, `FUXN_ROW`,
  `FUXN_COL`) and no other (D34 took out every other `FUXN_` name: a guard
  against the user's own variables); the terminal's name is one
  constant for `TERM` and the two capability answers; the README no
  longer says to restore from `.empty` (it runs only where no pane's
  folder is). Not taken, each the owner's to weigh because it removes
  something a user can name on or beside the keep-list: the four
  `arrange` presets and their keys (34 lines), OSC 99 notices (17),
  OSC 777 (5), `C-b w h` and `C-b w l` (10), `rotate` and `C-b g t`
  (10), `split` without `beside` (8), `TERM_PROGRAM_VERSION` (1).
- **D110. The tenth wave, the last of this run** (three new readers with
  the eighth's brief). Bugs, each with a test that failed first unless
  said: a mark goes with its cell when a character is printed before
  it in insert mode; an erase that takes the whole screen because half
  a wide character began it is a cleared screen (so is `CSI K` on a
  screen of one row); a wrap that waits on the alternate screen is done
  after a resize that keeps the width (fixed from memory of xterm; the
  referee has a case for it now, so CI asks xterm); a picture on the
  alternate screen keeps its place on a screen too small to show it,
  as on the main screen; half a line of `ctl` writes no screen; a batch
  of `ctl` commands is as old as its first (no test: a matter of
  time); a pane of one column asked to be narrower is not made wider;
  a press after the program asked for no mouse for a while is a new
  click; a menu on a terminal of one row leaves the bar; each run of
  `.restore` keeps a pane's text under a name of its own (no test: a
  race of two shells). Cuts: about thirty lines in small pieces (plain
  mechanics: the holder takes its words apart once; one search claims
  a pane's name; one fill; the layout's parser says when its text is
  too long; the pipe's end says the writer ended). Left: `FIONBIO`
  for the pty (the number differs per system: one more constant pair);
  a malformed OSC 7 naming this machine, after a good one in the same
  read, loses the good one (the emulator cannot tell which host is
  this one; the holder's fix is more than three lines); a kitty key
  release sent for a press that a full `in` dropped.
- **D111. A last audit for cuts of a whole thing, and a second panel of
  ten** (the owner, 2026-11-21: the line-by-line audits are dry; ask
  only for a whole command, file, state, boundary, check or part of
  the emulator worth about 30 lines or a class of bug). Four readers,
  each measuring in a copy of its own: three found nothing of that
  size (no command a recipe replaces, no file derivable from another,
  no state out of reach, no type that removes more lines than it adds,
  no part of the emulator nothing exercises, nothing in `fuxix` that
  std covers), one found the clipboard's codec. Two questions went to
  the panel (2 Opus, 4 gpt-6.2-sol, 4 gpt-6.2-astra; seven settles):
  1. The `clipboard` file as the base64 the program sent (38 lines,
     the codec gone): **kept as bytes, 9 of 10** (Opus 2/2, sol 3/4,
     astra 4/4). It would be the one file of a pane not readable as it
     is; a hand-written file would need `base64 -w0`; malformed text
     would reach the terminal. The dissent: fux could relay the
     payload and leave the coding to `base64`, one way to do it.
  2. The four `arrange` presets and their keys (34 lines): **kept, 10
     of 10**. They are part of tiling as tmux and fux 1 users know it,
     and the command rewrites the tree under the workspace's lock,
     which a plain script over the file does not take.
  Nothing of that size was taken: the tree is at the least this run
  could prove.
- **D112. The review of this run, round one** (six readers: the
  emulator; the holder and a pane's files; the viewer and the layout;
  `fuxix` and every unsafe block; the keys, scripts and agents' page;
  the docs against the code; two blind testers beside them). Taken,
  each with a test that failed first where one can be made: a wrap
  that waits on the alternate screen is forgotten when a shorter
  screen cuts its row (D110 kept it too widely); `pictures` lists only
  pictures with a row on the screen, on either screen; a styled text
  is read at any width without a sum that can overflow; a mark goes
  with its cell when a character before it grows wider in insert
  mode; a key's script is never told a size that is no size (the
  viewer knows its terminal's size from the start, and a terminal of
  no rows or of no columns leaves `LINES` or `COLUMNS` unset: the
  first shell starts with 24 or 80 for it, as before D102); the clipboard's stamp and
  bytes are of one open file (a copy could be sent twice); `C-b w n`
  starts the shell before it shows the workspace (D104's change let
  what was typed next find no pane); a picker's or editor's file that cannot be moved leaves
  mv's words until Enter. Taken back: D108's reading of a file again
  when its inode's change time moves (a chmod of `clipboard` or
  `notice` sent the copy and the notice again): a `colours` file that
  could not be read stays unread until its time of writing changes, as
  before D108.
  Not taken, and why: the earlier `state` of a pane whose start in
  place fails before its program runs stays, with the earlier
  `status` (the two agree; a test says a failed start leaves what the
  last program left; the README's sentence was narrowed to say so);
  allocations a reader counted on paths nobody measures as slower
  (one small list per line read from a FIFO; a tab's panes listed at
  each load of the layout; UTF-8 by std, D100); an overflow on a
  machine with a 32-bit `usize`, which `fuxix` is not written for.
  Tried and taken back in the second round: a rule in shares that
  would keep a part asked to be narrower from ever getting a cell
  more. Cells are handed out by rounding the running total of the
  shares, so no rule on one share holds for every row: the one tried
  still widened a part of one column among four, and refused a shrink
  that worked. As it stands (D107, D110): `size -N` never grows a
  share in a row of two, and in a longer row a part at the edge of
  the rounding can end one cell wider (the base did the same).
- **D113. The bugs of D100 to D112 by class, and what makes a class hard
  to write again** (the owner, 2026-11-21: "keep cutting code while
  making bugs impossible"; a second pull request, with the same bands:
  no more lines than before, nothing slower). Two readers sorted the
  76 bugs by the kind of mistake, each trying its remedies in a copy
  of its own and counting lines. The largest class, eighteen, is "the
  grid changed and something kept beside it was not told" (marks, the
  flag of a row that goes on, rows a resize lent, pictures, a wrap
  that waits). Taken, each at no more lines than it removed, with
  what refuses the old mistake now:
  - cells are moved sideways in a row by one function, `shift`, which
    moves the marks, keeps the flag on the first cell and mends halved
    wide characters (three sites did each by hand; five bugs were one
    of them forgetting one). The tests refuse a hand-written move, not
    the compiler: a row is a plain list of cells;
  - `place` decides itself whether a character is inserted (callers
    said so with a flag, and one said it wrong for an emoji that
    widens at a row's end): the old call does not compile;
  - `ESC # 8` erases through the erase every other whole-screen erase
    uses;
  - a reader's screen is built at its width by the constructor
    (`Term::sized`): the reader has no width sum of its own left;
  - a plain open to read (`File::open`, `fs::read`,
    `fs::read_to_string`) is refused by clippy, which CI runs: a FIFO
    there holds the caller (three bugs). With it went the known one, a
    FIFO where a workspace's folder is;
  - which workspace is meant is chosen and checked by one function
    that a command must hand its own name, if it has one;
  - the bar, a notice and the menu are lent only the cells they may
    paint: the menu reaching the bar's row by a sum does not compile;
  - the pty and the input waiting for it are one value: a hang-up
    cannot keep one without the other.
  Classes with no such remedy, and why, are in the pull request (each
  bound is another quantity; orders of steps cross processes; the
  races that are left are in sh). Not taken: "a press always begins a
  new click" (no line saved, and it changes what a second button
  does); continuation kept on the row instead of its first cell (the
  reflow reads it everywhere).
  Beside it, three passes of reading for bugs, each fixed with a test
  that failed first unless said: what is written to `in` after a
  `kill` is dropped, not left to block its writer; `SHELL` or
  `FUXN_WS` set and empty is not set; an emoji widened at a row's end
  in insert mode is put in before the next row's text; a row
  shortened keeps no sign of where its end was; a paste that began
  marked ends marked; a key's script is told the size the terminal
  has now; a sign outside ASCII is itself in both forms of a key, and
  so is a letter on a key that is no letter on a US layout; what is
  typed before an empty workspace has its shell waits for it, also
  after a key that went there; a notice is read from one open file;
  `.restore` starts no pane whose text it could not copy; `focus
  left|right|up|down` goes by the terminal's size when told it;
  `C-b w n` takes the next name when another took the one it found;
  `split` has one rule for what it splits beside, and a pane beside
  itself is refused in every form (the form that made a pane a tab
  of its own, the last, is `break`); `C-b S-c` keeps a command's
  last blank lines; a picker's or editor's file that could not be
  moved is removed; a sixel at the bottom of a scroll region that
  ends above the last row stays; `CSI Ps $ p` is answered; a start
  from a directory that is gone claims no name; a shell split from a
  pane that went meanwhile is shown in its own tab. The third pass:
  when the tab shown and an earlier one go together, the tab now in
  its place is shown (only `retain_panes` turns old places of tabs
  into new ones); what is typed after a key waits for every script
  started before it (the one slot for "the script waited for" is
  gone: two could be running); a menu too narrow for its count shows
  the mark alone; a picture kept past a smaller screen's edge goes
  with an erase that reaches that edge (one rule for which pictures
  an erase takes, for `erase` and the whole-screen fill). And the
  leftovers of D112's review that the base shared: five fixed, the
  rest each given its line in the README.

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
