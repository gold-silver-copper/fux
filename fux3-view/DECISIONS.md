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
    prompt is open, nor on the bar's row. Each file is read back into pixels by a fux-vt parser
    with `Feature::Pictures` once, while its screen names it.
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
