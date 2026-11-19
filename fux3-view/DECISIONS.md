# fux3-view: decisions

Append-only: one short entry per decision, newest last.

1. **Start from fux2's viewer, then cut.** Copied fux2-view's frame, panels, key table, grift
   thread and fux2's stored layout; deleted the socket client, connections, waits, tokens and
   patches. Why: the owner wants fux's look, and that code already has it.
2. **Cells come from a `fux_vt::Parser` per shown pane.** `screen.styled` holds SGR *changes*;
   the frame needs each cell's whole pen. A parser of the pane's size, fed each row after a
   cursor move with autowrap off, gives cells with their attributes, and fux-vt writes each
   cell's SGR back. Why: no second SGR reader to keep in step with fux-vt's writer.
3. **No pane titles.** fux2's layout let a tile carry a title; fux shows none on screen and
   fux3 panes have names. Lists show names. Why: less surface; fux3's labels already describe
   panes.
4. **A pane's liveness is its directory.** The viewer lists the session with
   `Session::names()` and never try-locks a pane directory: a try-lock that won could race a
   `fux3 start` claiming the name. A holder killed outright shows as running until the next
   `fux3` command clears it, as `ls "$D"` does. Why: never break a start to look.
5. **`split PANE DIR` with no third pane starts a shell.** The owner's example
   `echo "split build right" > "$D/.view/ctl"` has one pane, so the only reading is "a new
   shell to the right of build". The shell is `fux3 start shell-N` (the first free N), run
   by whoever applies the line (a viewer or `--do`). Why: the example must work as written.
6. **`.view/ctl` lines go through grift.** A viewer that reads a line calls the library's
   `layout-command`, which by default applies it and fits the panes it changed, once. `--do`
   applies the same grammar to the file directly and fits nothing (no viewer, no tile size).
   Why: the fit is policy, and the brief puts it in the grift library.
7. **Pane names skip names a tile still shows.** A gone pane's tile keeps its name on screen
   (`shell-1 gone`); a new `shell-1` beside it read as the same pane. `start-new` takes the first
   `PREFIX-N` no pane and no tile uses.
8. **`Reread` re-reads at once.** A binding that edits and then shows the tab it made
   (`move-to-new-tab`) failed: the viewer had not read the edit yet. Re-reading on the message
   also made the deferred-fit list and the wait-for-a-focused-pane machinery unnecessary; both
   are gone.
9. **Cuts for the 6,000-line cap** (the owner's bound): no pane titles, no `replace`, no
   `tab match` (a filter is set by `tab new … match`), no percent border amounts, no live drag
   preview (a drag applies on release), and the verbs fux binds to no key (`literal`,
   `go-to-tab`, `last-tab`, `last-workspace`, `previous-pane`).
10. **Pictures redraw everything when they change.** A sixel's pixels stay until the cells under
    them are written, so when the set of pictures drawn changes (one moves, a menu opens over
    it) the frame is drawn whole, then the pictures. No picture is drawn while a menu, list or
    prompt is open, nor on the bar's row. Each file is written to the terminal as it is, read
    again at each redraw, and only if it is one sixel alone: it starts with `ESC P` and holds no
    `ESC` but its closing `ESC \`, so no other sequence can ride in a picture file. (An earlier
    text here said each file was parsed back into pixels by fux-vt; the code never did that, and
    the check above gives the same guarantee for a few lines.)
11. **Tests drive the viewer inside a pane of a second fux3 session.** Keys are written to that
    pane's `in`, the viewer's screen read from its `screen`, its terminal resized with
    `resize` on its `ctl`. No pty code in the tests, and they read exactly what a person would
    see. Writes to FIFOs in tests are non-blocking, so a gone holder is an error, not a hang.
    `cargo test -p fux3-view` builds `fux3` first if the run did not (`$CARGO build -p fux3`).
12. **Clearly under 6,000 lines (owner, 2026-11-19): what went.** Repeating key groups, the
    `[hidden]`/`[cut]` corner, the notice when a program ends (its tile says so), holding keys typed
    while the keys load (they now wait in the terminal; loading is bounded per part, so detach
    still comes within those bounds), `swap` and `C-b a s`, `send-key`, sixel cropping (a picture
    shows only whole; the owner's prompt allowed this), and mouse border dragging with the
    `if VERSION` guard it needed (a click still focuses; `C-b r` and `border` move borders). Kept,
    as the owner listed: the look, the layout file, `.view/ctl`, the menu from bindings, lists and
    prompts; and the pictures, part of fux's look. Why drag over pictures: the keyboard and the
    `border` command cover what a drag did; nothing else draws a picture.
13. **The painter is one busy flag.** It is set before the writer thread can see a frame, and the
    writer clears it when the frame is out: the next frame is made only then. An earlier order
    (set after the hand-over) could leave it set forever and stop painting; a test run caught it.
14. **A pane just started is waited for.** `fux3 start` can return before the pane's first files
    exist: the holder hands its first screen to its publisher thread and says `ok` at once. About
    one `C-b c` in five failed with `history-1/status: No such file`. The viewer now waits for
    `status`, within the start's own time bound (core need written in the status file). A new
    life under a reused name also drops what the viewer kept for the old one (its open `in`, its
    queued keys, its screen).
15. **One layout per workspace, a workspace a directory (owner, 2026-11-19).** The layout file
    lost its workspace level: `$D/WORKSPACE/.view/` holds the workspace's layout (tabs and
    splits; header now `fux3-layout 2`, no old file read), its lock, `ctl` and `errors`. A viewer
    shows one workspace; switching opens the new one's `ctl` (taking a regular file's lines, as at
    start), drops the old one's, and starts from its files again, keeping what it remembers per
    workspace (the tab and focus it left; memory, never a file). Why derive the list from
    directories: they are the one source of truth the core already keeps; a second list of
    workspaces in a file could disagree with them.
    - **No rename.** Renaming the directory moves live holders' directories from under them
      (they write by path), so it is not safe; a workspace is called what it was made as.
    - **No reorder, no moving a pane to another workspace.** The order is the names'; a pane's
      workspace is fixed when it starts (the core's rule).
    - **New** asks for a name, refuses one that exists, starts `NAME/shell-1` (which makes the
      directory) and shows it: the shell has no tile, so routing puts it in the one tab.
    - **Close** asks, ends every pane (`kill` on each `ctl`), waits up to 12 s for them to go,
      then under the session's lock (so no `fux3 start` can put a pane in it meanwhile) removes the
      directory, `.view` and all. A pane that will not end is named and the workspace stays.
      Removing, not leaving an empty directory: an empty one would stay in every list.
    - **A workspace whose directory goes** (closed here, by another viewer, or `rm -r`) gives way
      to the first left by name, else a new `main`: one rule for every way it can go.
    - **A run acts on the workspace shown when it was asked for.** A run's context carries the
      workspace's name, so a `.view/ctl` line queued before a switch still acts on its own
      workspace, as does a binding that switches workspaces partway through.
    - `start-new` takes `WORKSPACE/PREFIX` to start in another workspace (as `fux3 start` takes
      `WORKSPACE/NAME`); `session-dir` became `workspace-dir`; `(workspaces)` lists names;
      `show-workspace` takes a name.
    - Left as it was: a `.view/ctl` FIFO that no viewer reads makes a writer wait, as before
      (a viewer that switches away, like one that ends, leaves it). Removing it on leaving would
      break another viewer still reading it.
16. **Review round 1 (2026-11-19): what changed, and why.**
    - **Paste files are kept their whole 10 s.** The viewer guessed which `commands` count was its
      paste, but every writer of the pane's `ctl` moves that count, so a guess could remove the
      file before the holder read it. A file now goes 10 s after its `paste`, or when the viewer
      leaves (a paste under 1 s old gets the rest of that second first). The files live in the
      viewer, not its panes, so a workspace switch keeps them too.
    - **One viewer at a time reads a workspace's `.view/ctl`**: the one holding an flock on
      `.view/ctl.lock`, taken without waiting and tried again at each rescan, released when it
      leaves or switches. Two readers of one FIFO can each take part of a line. Only the holder
      makes, opens or removes the FIFO, so the layout lock no longer guards it.
    - **A run keeps its workspace.** The viewer knows the run in flight (one at a time) and the
      workspace it was asked for in; after a switch, that run's sends, focus moves, fits and
      lists are refused out loud (the list's callback is dropped), and a `.view/ctl` line's
      refusal goes to its own workspace's `errors`. Refused rather than delivered: the panes of a
      workspace not shown are not the viewer's to type into. Verbs still act (they are the
      viewer's own, e.g. a switch). The configuration's load ends with its own message, not a
      run's, so it can no longer start a second run while the first is in flight.
    - **Chord hops are capped at 8**, counted from the key along both paths (a verb bound to
      `chord`, and a binding that runs `(act "chord" …)`); the ninth is refused in the bar.
    - **A switch dismisses an open list or prompt**, so its answer can't act on the new workspace.
    - **Layout edits are checked by the reader before they are written**; that check replaced the
      separate size check, as the reader checks size too.
    - **Closing a pane or tab ignores only `pane-gone`**, a new error kind raised when the pane's
      directory went or another life has its name; other failures (a full `ctl`) are said and the
      tile stays. A holder that died without its directory being cleared counts as a failure,
      not as gone: the next `fux3` command clears it.
    - **Tab names**: a bare `tN` is the tab with that id when one exists, before any title; a
      quoted word is never a keyword. Quoted words are matched with a `"` mark no bare word can
      hold, so the one grammar table stays.
17. **Templates: bare tiles (owner, 2026-11-19; the analogies decision, "use the sh script
    format").** The layout file takes a bare `pane NAME` (and `zoom NAME`) beside `NAME@ID`.
    - **A bare tile is whichever pane has that name now**: bound when the shown layout is derived,
      `NAME gone` while there is none, and each new life of the name takes it. A `NAME@ID` tile
      keeps its rule (a new life of the name does not take it). A live pane that has an `@ID` tile
      of its own does not also bind a bare one: one pane, one tile.
    - **An edit keeps a bare tile bare.** The edit binds bare tiles to the live panes, applies the
      command as before, then makes those tiles bare again wherever they went (moved, swapped,
      zoomed). So commands need no new case, and a template stays a template while it is used.
      Tiles the viewer adds (a split, a routed pane stored by an edit) are `NAME@ID` as before.
    - **Stored as a `PaneRef` with an empty id**, not a new tile type: the tree's operations all
      compare `PaneRef`s, so none changed. Names are never empty, so a bare tile never equals the
      swap's placeholder.
    - **No viewer command makes a template**: the README gives a one-line `sed` that strips the
      ids, and the loop that starts every recipe. Lines: production (without in-file unit
      tests) 5,760 → 5,805; whole files 5,978 → 6,065, of which the new test is 32. Nothing was
      cut: the ceiling is counted on production lines, as the brief states.
18. **Review round 2's viewer fixes (2026-11-19, the last round).**
    - **A viewer-started shell names its program**: `start` passes `-- "${SHELL:-/bin/sh}"` when
      given none, so a reused `shell-N` never runs that name's old recipe. Done in `start`
      itself, so every caller (`C-b v`/`s`, `split PANE DIR`, new tab, new workspace, a
      config's `(start-new P)`) gets it; `fux3 start NAME` by hand still runs the recipe.
    - **The history pager removes its own recipe** before it ends. A pager closed with
      `C-b x` is killed before it can; the README's template step removes `history-*`.
    - **`NAME@` names a bare tile no pane has.** `focused` and `panes` give it so for an
      unbound bare tile; a kill of `NAME@` finds no such life (`pane-gone`), and `close NAME@`
      takes only that tile. Chosen over a special case in `close-pane`: every command that
      names the focused pane (zoom, move, border) now acts on the tile shown, not on the live
      pane of that name in another tab. Bare `NAME` keeps its rule (the live pane's tile).
    - **`write_first` writes only a missing layout**, checked under the layout's lock.
    - **A started pane's `status` is read once**: the core publishes it before `ok`.
    - **A chord dispatched by a run whose workspace is no longer shown is refused**, as its
      sends, focus and panels are, rather than started with the new workspace's view. Carrying
      the old workspace into the new run would have given it the old workspace's name with the
      new one's layout snapshot.
    - Lines: production 5,999 → 5,999 (comments tightened to fit).
19. **`.view/ctl` and `.view/errors` are gone; a layout command is an edit of the layout file
    (the panel's decision 3, 2026-11-19).** `fux3 view WS --do LINE` edits the file under its lock
    with or without a viewer; a refusal is its stderr and exit 1. Deleted: the FIFO, its reader and
    hand-off (`ctl.lock`), the lines a regular file there held, the refusal queue, the run kind
    that called a function by name, and the library's `layout-command`.
    - **A running viewer fits what an edit from outside changed.** `--do` knows no viewer's size
      and fits nothing. A viewer that finds the layout file changed at its once-a-second look
      (not at the read its own edit asks for) fits the panes whose tiles differ between the old
      file and the new one, with the same panes live. With no viewer nothing is fitted. Cost,
      accepted: of two viewers of one workspace with different sizes, the one that did not make
      the edit also fits, so the last to look sets the size. A viewer's own `arrange` (no fit) read
      by the once-a-second look before its own read would be fitted too: a window of microseconds.
    - **A bare zoom is resolved after routing** (decision 7): when what is shown is derived, a
      bare `zoom NAME` whose tab holds a live pane of that name, routed or placed, zooms it. The
      file keeps `zoom NAME`. `zoom` toggles on what is shown, so it restores a bare zoom too.
    - **`C-b c` gives `$EDITOR` the history and then the screen** (owner, 2026-11-19): the older
      generation, the history, then `screen` without line 1 and without trailing blank rows, in
      `$FUX3_DIR/.history-PANEID/history-and-screen` (a directory of its own, 0700, so an
      editor's swap file goes with it; removed when the editor ends or the pane is killed). The
      editor: `$EDITOR`, else `$PAGER`, else `less +G`; `vi`, `vim`, `nvim`, `nano` and `emacs` are
      given `+LASTLINE` (a number, not a bare `+`: all five take it), any other editor the file
      alone. The directory is in the session's, not the pane's: the core removes nothing it does
      not know from a pane's directory, and a pane's own path goes stale after `mv`.
    - **`(when-gone PANE FUNCTION)`: the tile closes once the pane is gone.** The pager's script
      used to close its own tile through `.view/ctl` and then end itself. A tile closed while
      its pane still lives is routed straight back for a moment, and the neighbours were fitted
      to that moment's layout (driven: after `C-b x` on the pager, its neighbour stayed 39
      columns wide in a 59-column tile; after `C-b x` on one of two panes the other stayed 59 of
      120). Now the script only ends itself, and the viewer that opened it runs the callback at
      its next look after the pane's name is gone (or another life has it): `close-tile`, which
      edits and fits. `close-pane` (`C-b x`) does the same: kill, then close the tile when the
      pane is gone. Chosen over `--do "close …"` from the script, which needs the workspace's and
      the pane's names (stale after `mv`) and closes the tile while the pane is alive. Cost: a
      viewer detached while a pager is open leaves its tile, marked gone, to be closed by hand;
      at most 64 panes are watched.
    - Tests deleted with `.view/ctl`: `two_viewers_of_a_workspace_split_no_ctl_line` (there is no
      FIFO to split). `a_ctl_line_changes_only_the_layout_of_its_workspace` kept its property and
      its second half; it sends its lines with `--do` and is now
      `a_layout_command_changes_only_the_layout_of_its_workspace`.
    - Lines: production 5,999 → 5,850.
