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
