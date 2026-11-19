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
