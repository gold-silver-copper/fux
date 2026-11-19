# fux3's decisions

Append-only. The panel is the ten-seat panel of 2026-11-18 (2 Fable, 2 Opus,
3 gpt-6.2-sol, 3 gpt-6.2-astra); its ballots are in the owner's notes
(`notes/projects/fux/fux3-2026-11-18/panel/`, `panel-r2/`). Where the owner
overruled it, both are recorded.

## Decided before the build

- **D1. A new crate, `fux3`, beside fux and fux2, with its own holder over
  `fux-vt` and `fuxix`.** It shares no code with fux2 or fux and knows
  neither. Panel 10/10.
- **D2. No socket.** What needs the holder to act goes through `ctl`. Owner,
  overruling the panel's 7/10 for a small one-request-per-connection socket
  (round 2: Opus 1, sol 3, astra 3; files: Fable 2, Opus 1); the socket's main
  reason was a guarded send answering at once, and the owner removed the
  guarded send. Strongest dissent, which the owner's choice follows: a socket
  makes fux3 a second fux2 and needs a client that speaks sockets, which a
  bare macOS shell lacks.
- **D3. `in` and `ctl` stay two channels**, so typed bytes are never read as a
  command. Owner.
- **D4. How a client learns what happened, with plain tools**: `status` shows
  the effect and counts commands, every refusal is a line in `errors` naming
  the command, a complete kill removes the directory, an incomplete one leaves
  `status` saying what is left and `kill` may be sent again. Owner.
- **D5. The screens are published at once on the first change after a rest,
  then at most once per interval, and once more at rest.** Panel 10/10, with
  the owner's first-change rule. Never torn; `screen` and `screen.styled`
  carry the same version on line 1.
- **D6. `history` is plain text, not the program's raw bytes.** Owner,
  overruling the panel's 7/10 for a raw log (2 Opus: rendered lines; 1 astra:
  an escaped transcript): raw bytes replay escape codes in the reader's
  terminal and let a pane's program attack whoever tails it.
- **D7. The program's exit is a fact in `status`; its last screen stays until
  `kill`; writes to `in` after it are taken and dropped with a line in
  `errors`; `in` is never removed while the directory stands.** Owner.
- **D8. Sixel pictures on by default**, laid out in a cell size from the
  starting terminal or a stated default, with options and `cell W H`. Owner.
- **D9. Room for the kitty image protocol later**: another kind of picture is
  another extension in `images/`. Owner. Not in this PR.
- **D10. No guarded send.** Line 1's id and version stay, so one can be added
  without changing a file. Owner.
- **D11. `fux3 wait NAME [--ms N]`**, as macOS ships no file watcher. Owner.
- **D12. The binary is small**: `start`, `dir`, `wait`, `ls` (lists live
  panes; every command clears dead ones' leftovers), and `view`. Owner.
- **D13. The session directory should be in memory**: Linux by default; macOS
  measured and documented, with `FUX3_DIR`. Owner.
- **D14. Nothing plans fux's or fux2's removal.** Panel 10/10.
- **D15. The viewer is in this PR, as its own crate `fux3-view`, for fux3
  only.** Owner, overruling the panel's 10/10 for no viewer in the first PR.

## Decided while building (by the implementer, with why)

- **D16. The session directory is `FUX3_DIR` itself**, else
  `$XDG_RUNTIME_DIR/fux3` (Linux) or `~/Library/Caches/fux3` (macOS). One
  session per directory: no session names, as a second session is a second
  `FUX3_DIR`. Linux with no `XDG_RUNTIME_DIR` is refused rather than falling
  back to a disk directory: the owner wanted the session in memory, and a
  silent fallback would put it on disk.
- **D17. Writing the files runs on a thread of its own in the holder**, so a
  slow or full disk never stops the holder answering its program or reading
  `ctl` (a risk the prompt names). The loop hands over the newest screens; one
  not yet written is replaced; history text waits in order up to 4 MiB.
- **D18. Line 1 is the same in both screen files and carries the size and the
  cursor too** (`id= version= size= cursor= cursor-shown=`), as `key=value`
  words a reader may skip. The viewer needs the cursor and the size with the
  screen they belong to; putting them in `status` would pair them with another
  version.
- **D19. Pictures in `screen.styled` are lines after the rows**, naming the
  file and the part shown. An escape sequence would be invisible to `cat` but
  is a code fux3 would make the reader's terminal take; a plain line is honest
  and costs a line per picture.
- **D20. Refusals carry the command's number** (`N LINE: WHY`), `N` being
  `status`'s `commands` once it was taken: with only plain tools, a client can
  tell its own command's outcome by reading `commands` before and after.
- **D21. `ctl` takes `paste PATH` and `signal NAME` besides the four the prompt
  names.** The viewer sends pastes, which must be bracketed in the mode the
  program is in at the moment it gets them, which only the holder knows; and
  fux's "terminate the running command" needs a signal, which agents need too
  for a hung program.
- **D22. The publish interval is 40 ms** (the panel suggested 30 to 50).
  Measured: a key's echo reaches `screen` in about 2 ms because the first
  change after a rest is published at once, so the interval only bounds a
  flood, at about 100 file writes a second.
- **D23. No raw output log**, not even as an option: it was allowed, off by
  default; nothing in the frame needs it, and it is the attack D6 removes.
- **D24. The default picture budget is 16 MiB of pixels** (fux-vt allows 96):
  a 4K picture is 33 MiB, a screenful of ordinary pictures a few MiB; with 128
  panes, 96 MiB each would be 12 GiB.
- **D25. A `send` is at most 16 MiB**: a large paste or file, still well within
  memory, read whole before anything is queued.
- **D26. The terminal keeps 2 MiB of history between looks**, and the holder
  looks after every read of at most 16 KiB, so no ordinary line is lost
  between looks even under a flood (tested with 1,000,000 lines by a tester).
  First 8 MiB and 64 KiB reads; a real-use tester measured a resize under
  `yes` taking 0.45 s, as a resize reflows the terminal's whole history: now
  about 0.1 s. Output built to fill rows cheaply can still outrun it; the
  loss is counted in `history`.
- **D27. `fuxix::file::private_fifo`** is the one primitive added to fuxix
  (`mkfifo`, mode 0600): the workspace forbids `unsafe` outside fuxix.
- **D28. A pane's kill ends everything started inside it, panes included.**
  The viewer's builder found that on Linux a holder adopts the holders
  started from inside its pane, so killing the outer pane kills them. I first
  spared other holders; the owner, shown the finding: "maybe we should
  preserve this behavior?" — one kill ends a task and all the panes it
  started, with no special case. Kept, stated by a test; on macOS an inner
  pane's holder left the session and outlives the outer pane (README).
- **D29. A workspace is a directory, and a pane a directory of its
  workspace** (`$D/rig/build/`). Owner, 2026-11-19: "what if we represent
  workspaces as folders... so that pane files are stored in folders", then
  "I do like this". `fux3 start [WORKSPACE/]NAME` (default `main`); `ls "$D"`
  lists workspaces; the viewer keeps one layout per workspace, in its
  `.view/`, and shows one workspace; workspaces are listed by name, so they
  have no order to change, and a pane does not move between them. An empty
  workspace directory is left alone: it is how `mkdir` makes one. Programs
  get `FUX3_WORKSPACE`, `FUX3_PANE` and `FUX3_PANE_DIR`.
- **D30. `history` is capped by rotation, not by rewriting.** First the file
  was replaced by its newer half at the cap; a separate panel's spike found
  that `tail -F` then prints the kept half again. Now each of `history` and
  `history.1` holds at most half the cap: past it `history` is renamed
  `history.1` and a new one starts, so a follower by name sees each line
  once, and the two together stay under the cap with no gap (tested).
