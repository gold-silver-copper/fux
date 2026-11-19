# Decisions

Append only.

## D-1: what fux2 is (the owner, 2026-11-18)

Create fux2 beside fux, resting on the same `fux-vt` and `fuxix`. It has a viewer.
It lives in the fux repo as new crates, and runs on Windows, Linux and macOS. The
core speaks plain verbs and links no language; keys and custom commands are the
viewer's, configured in grift.

## D-2: the design (a panel, 2026-11-18)

Three designs were written apart and ten seats scored them blind. This one won 7
of 10: Opus 0 of 2, gpt-6.2-sol 3 of 4, gpt-6.2-astra 4 of 4. The two Opus seats
and one sol seat preferred another design, mainly for a smaller daemon with keys
encoded by the client. Two amendments passed with four or more ballots: an atomic
resize, and grift bindings that cannot freeze the viewer.

## D-3: a claim is a lock on the pane's directory (Unix)

A panel seat noted that cleaners remove old lock files, after which a second
process could claim the name. A directory holding a live socket is not removed by
them. Windows locks a file, which cannot be deleted while held.

## D-4: a resize prepares on a copy of the parser

`Parser` is `Clone`, so the copy needs no change to fux-vt. It costs one copy of
at most 16 MiB of history, on an explicit and rare request.

## D-5: a send is a list of parts

`\n` must be Enter as the program's terminal mode sends it, in the same atomic send
as the text around it. So a send is literal bytes, keys and pastes in order, encoded
by the pane.

## D-6: `--settle` and `--until-line` on `read`

A wait wakes on the echo of the typed line, so "first change" is often too early.
`--settle` (default 100 ms) waits for quiet. `--until-line` matches a whole row,
which the echoed command does not.

## D-7: tokens remember their rows; only issued tokens are kept

Keeping row ids for the 64 tokens actually handed out, not the last 64 versions,
means a busy pane does not forget what its readers hold. With ids, a patch is exact
whatever moved, including scroll regions that the scroll count alone misdescribes.

## D-8: programs cannot change the palette

`Feature::Palette` stays off. A palette change would recolour rows without changing
them, which no patch carries, and carrying it is more than this needs.

## D-9: a job that forbids leaving is reported, not refused (changes the design)

The design refused to start. GitHub's Windows runners run every step in such a job,
so the README's example could not run there at all, and the user would have no way
forward. `start` now says so on stderr before the program starts, and the pane lists
it among its limitations.

## D-10: bindings use the `fux2` command

Windows allows one polling thread per process, and a binding must be stoppable while
it waits on a pane. A child process satisfies both, and there stays one way to reach
a pane. `NAME@ID` keeps a binding on the pane focused when its key was pressed.

## D-11: keys wait for the configuration, built-in keys do not

A key pressed while `view.grift` loads may be about to be bound, so it is held, up
to 256 keys and the load's two seconds. Built-in chords act at once, so detaching
never waits.

## D-12: at most 12 parked waits of 16 connections

The other four slots cannot be held by waits. Idle connections are displaced for a
newcomer, and a connection mid-request is bounded by its five-second limit, so a
control request always gets through within that bound.

## D-13: scrollback is text and a pager (the owner, 2026-11-18)

The owner chose this over a scroll mode: `read --history-text` prints the retained
history and screen, and `C-b [` opens it in a pager in a new pane. The daemon gains
only larger history pages and row numbers that run on through the screen. Wrapped
lines are joined using `Row::continues`.

## D-14: workspaces and more keys, all in the viewer (the owner, 2026-11-18)

A workspace is a named list of tabs in the layout file, and a tab stays a filter on
labels, so the daemon and protocol do not change. Default chords follow tmux where
it has one: `n p l c [`, digits, `( )`, with `O` and `L` for the two "other way"
moves it spells differently.

## D-15: the default keys are grift, and most of them are verbs

The owner wants them rebindable and the pager key to be a worked example, so they
live in `defaults.grift`. But a key that ran on the language's thread could not act
while a binding is stuck, which would break detach. `bind-verb` therefore binds a
chord to an action the viewer's own thread performs. `C-b d` is also present in Rust
before anything loads, so there is always a way out.

## D-16: `fux2 page` rather than a shell pipeline

The history pane's program must work without a shell's quoting on any system, and
must remove its own pane only if the pager ran. If no pager can start, the pane stays
and shows why.

## D-17: review round 1 (2026-11-18)

Six reviewers and two testers. What changed in the design, in short:
- A history answer fills to the answer cap and sends the screen only with the
  last history row, so a pane's whole default history comes in one observation;
  rows let go before the first one asked for are skipped, and the answer's first
  row number says so. `gone` no longer comes from a history read.
- `--until` and `--until-line` count only rows changed since the token.
- `C-b [` runs `fux2 page ... --close-pane`; plain `fux2 page` never removes a pane.
- An ended program is reaped when nothing it started is left, so ended panes
  leave no zombie; otherwise it stays unreaped for `kill`.
- A connection gives up its slot to a newcomer only after a second of asking
  nothing, so one just greeted is not evicted before its request.

## D-18: review round 2 (2026-11-18)

- A wait for a line compares each look with the last screen it looked at, not
  with the token it was given. A look whose screen the pane has forgotten, or
  whose two reads saw different screens, matches nothing and the wait goes on:
  a timeout (exit 5) is the cost, a false match never is.
- D-12 is corrected: a control request gets a slot within 5 s when the others
  are waits or slow requests. When some are slow readers of large answers, it is
  30 s, the most an answer may take to drain. No slot is reserved.
- The history pane removes itself by `NAME@ID` (from `FUX2_PANE_ID`), so it can
  never remove a later pane of its name.
- An ended program is reaped when its last descendant goes, at that child's
  signal, not only when the screen freezes.

## D-19: Linux and macOS only (the owner, 2026-11-18)

The owner: "Feel like we should get rid of windows supports cause it keeps causing
issues". This supersedes D-1's "window linux mac": fux2 runs on Linux and macOS.
Its Windows code is deleted (named pipes and marker files, the kept copy of the
program, the elevated session folder and config rule, console signal delivery,
the job-breakaway report of D-9), and building it for Windows stops at one
`compile_error!`. The neutral `fuxix` additions stay, since fuxix still serves fux
on Windows. fux's own Windows CI is as it is on master; fux2 is not built there.

## D-20: the full viewer (a panel, 2026-11-18, two rounds)

The owner: "I think we should still keep those commands that fux has, and have
the dynamic command viewer ... it has all your keybindings, and then when you
do prefix key it shows them, and it has it in like a proper hierarchy", and on
workspaces and tab keys, "yes i want them". Ten seats (2 Fable, 2 Opus, 3
gpt-6.2-sol, 3 gpt-6.2-astra) decided; ballots are in the owner's notes
(`fux2-2026-11-18/panel2`, `panel2-r2`).

1. The layout is one file in the session directory, written by viewers and
   command-line clients and reread by all, so every viewer of a session shows
   the same workspaces, tabs and splits. It has its own lock with a bounded
   wait, and writes replace it whole and atomically. 9 of 10; the tenth
   seat's separate lock is adopted.
2. Anyone can change the layout through the command line (`fux2 layout`), and
   the viewer's keys call the same operations. 9 of 10.
3. Rust gives a few primitives (the layout's operations, a panel, a line, a
   list, the verbs, focus); the commands are a grift library shipped with the
   viewer, which the user's config extends or replaces. 10 of 10.
4. The prefix menu is drawn from the bindings themselves: a binding carries a
   caption and a heading, a key bound to a group opens that group's menu, and
   nothing else defines the menu. 10 of 10.
5. Viewing never resizes a pane; your own layout command does. The shipped
   split, move and border commands fit the tiles they changed, once, in the
   acting viewer only (a drag on release), in the library, so a config can
   drop it. Round 2, 6 of 10; four wanted resizing only on an explicit fit key,
   because every resize reflows history and makes an agent's `send --if` go
   stale. The README says that cost.
6. Whether fux2 replaces fux is not decided now: no bridge to fux, no parity
   checklist. 9 of 10.
7. One PR for all of it. Round 2, 6 of 10; the four others wanted the stored
   layout proven first, so it was built and tested headless first (two
   writers at once, a writer killed mid-write) before any viewer key used it.

The panel's majorities, for the builder: a tile names `NAME@ID` (7 of 10); the
TOML `--layout` file is deleted with no import (10 of 10); a pane is in one
tab, an explicit placement wins and an unplaced pane goes to the first tab in
stored order whose filter matches, with a catch-all (10 of 10); placing a
pane is a layout operation and starting one is another, and a placement that
fails after a start is retried once and then reported, never undone by a kill
(10 of 10). Added by the orchestrator and agreed by the owner: the layout has
a version every write advances, and every edit takes `--if VERSION`.

## D-21: decided by the builder of D-20

- **A line-based text file**, not JSON or TOML: fux2 links no parser, and a
  strict reader of its own lines is less code than either. It names its line
  when refused.
- **fux2's own small tree** in the `fux2` library. fux's tree in `fux-wire` is
  tied to fux's ids and wire format, and moving it would touch fux.
- **The catch-all** is any tab without `match`, as the default tab `all` is;
  when every tab has a filter, a tab `other` is shown at the end of the first
  workspace, and stored only when an edit names it.
- **Routed panes stay routed** until an edit touches their tab's tree, so a
  pane killed from the command line before then leaves no missing tile.
  Renaming, filtering and zooming a tab do not store its routed panes: storing
  them would pin them there when the tab's filter later changes.
- **The pane an edit places is not routed during that edit**, so starting a
  pane and placing it moves no other tile, and the panes `changed` names are
  exactly those the placement moved.
- **Borders move by cells as seen in an area** (`--area`, which the viewer
  passes), or by a percentage, rounded up so a cell asked for is a cell moved;
  each side of a border keeps at least 5% of its split.
- **`move PANE DIRECTION` swaps** the pane with its neighbour that way: one
  rule for any tree, where "beside its neighbour" would need a rule per shape.
- **Lists and prompts take a callback** and return at once; the choice runs as
  a run of its own. Parking the grift run would have needed a second run model
  and a paused clock; this keeps one run at a time, each five seconds.
- **Keys follow D-14 where #462 set them** (`n p l c [ ( ) L O f d`, digits)
  and fux's keys elsewhere (`v s x z`, `a`, `t`, `w`, `r`, `m` menus, `:` for
  the command line as tmux, `?` for the key list, `;` for the last pane,
  arrows for direction, `g` for a pane picker), since `l`, `c` and `b` already
  meant something else here.
- **Focus moves only within the shown tab**; `(focus PANE)` shows the pane's
  tab first. Focus, the tab shown and the workspace shown are each viewer's
  own, as before; zoom is stored, so every viewer of the tab shows it.
- **A pane's title** is stored on its tile (`fux2 layout title`): the pane's
  name is its daemon's claim, which the layout does not change.
- **Respawn is left out**: a pane does not say what program it runs, so the
  library cannot start it again without a daemon change, which this PR does
  not make.
- **`fux2 page --close-pane` removes its tile** before ending its pane: the
  pane was made for the pager, so its tile has nothing to keep.
