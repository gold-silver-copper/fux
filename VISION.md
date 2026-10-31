# fux: the vision

Merged on 2026-10-30 from ten drafts (linguist, ontologist, semanticist,
computer scientist, AI researcher, Lisp expert, Forth expert, architect,
power user, strategist) in
`~/code/notes/projects/fux/vision-2026-10-30/`. This page changes only by
an owner decision recorded in `DECISIONS.md`.

## The goal

**A small terminal workplace where about ten agents do real work all day,
and a person watches and steers.**
- **fux** keeps programs running in panes, and lets agents start, read,
  wait on and drive them.
- **The editor** holds text, and every command it has is a fuxth word that
  can be read, changed and redefined while it runs.
- **fuxth** is the one language both are driven, configured and extended
  in.

Every action is bounded, acts only where it was allowed and on what was
seen, says what it did, and never guesses.

The test of it is not in the code. It works when about ten agents do the
owner's real work in it for a week, and their own records show few wasted
calls, no damage outside their scope, and every obstacle turned into a fix.

## Who it is for

1. **Agents first.** Claude, Codex and whatever comes next, each in its own
   tab, driving fux and the editor one command or one small fuxth program
   at a time. They are the daily users and the source of all evidence.
2. **The person who watches and steers.** The owner attaches to see the
   agents' work, steps in when they choose, and is never asked a question:
   a panel of ten agents settles those.
   The owner doesn't have to live in the tools for them to succeed.
3. **People who want to live in it.** Welcome, and the same words serve
   them. A feature that only they need (themes, mouse menus, a richer bar)
   waits until a scenario asks for it.

"Agents first" never means code that knows about a particular agent. There
is one interface. It is designed for a reader who pays for every byte, and
whose next turn starts with a fresh shell.

## The things that exist

This list is the anchor. **Adding, removing, splitting, merging or renaming
a row is a vision change** and needs an owner decision. Everything else
(words, options, defaults, internal types) is ordinary work.

| Thing | Belongs to | Identity | Lifetime |
|---|---|---|---|
| value | fuxth | none: a value is what it is | while held |
| word | fuxth | its name plus which definition (version) | its dictionary's |
| dictionary | fuxth | one per host instance | as long as its host keeps it |
| run | fuxth | none | one unit of source, with its steps and bytes |
| server | fux | its socket and its server run | start until told to stop |
| workspace, tab, pane | fux | `+N`, `@N`, `%N`, plus server run; a given name or none | until their last child closes; a pane until killed |
| program | fux | a pane plus its generation | start to exit; a respawn is the next one |
| client | fux | `cN` plus server run | attach to detach |
| journal | fux | events numbered within a server run | the server run's, bounded |
| clip | fux | `bN` plus server run | until pushed out (today's "paste buffer") |
| record | fux, at its edge | carries its server run, and a version if it read a pane | while held: one immutable observation |
| buffer | editor | its given name | until killed |
| window | editor | its place in the editor's layout | until closed |
| keymap | fux and the editor, separately | per client in fux; by name in the editor | its program's |

**Not things, on purpose:** an agent (it's a program in a pane), a person
(a client), a command (a word), a hook (a global holding word names), a
capture (a record), a version (a value read from a program), a label, a
layout, a mode, a setting.

## Principles that never change

1. **Facts, never judgements.** fux reports what happened: a version moved,
   a program exited with 7, a text is on the screen. It never decides that
   a program is ready, idle or done. Nothing is guessed: no made-up names,
   values, defaults or spelling suggestions.
2. **Act on what you saw.** An action can carry the version it was based
   on, and is refused as stale if the thing moved. This holds for panes
   today and must hold for editor buffers.
3. **Act only where you were let in.** A client can be confined to a scope.
   Acting outside it is refused out loud, so ten agents can share one
   server without closing each other's tabs or the owner's.
4. **Every failure says what already happened.** With the whole-program
   checker gone, a program can fail after it acted. Its error lists the
   actions that already took effect.
5. **Hard bounds, refused out loud.** Memory, steps, sizes, queues, history,
   and the whole agent population's processes, builds and spending. Crossing
   a bound is an error with a code, never a slowdown or a crash.
6. **Deterministic, with its inputs stated.** The same source, the same
   starting state and the same host answers give the same result. The order
   in which competing agents arrive is an input, not something fux makes
   repeatable.
7. **One language, mechanism in Rust, policy in fuxth.** Atomic changes,
   bounded observations, engines and OS facts are Rust. Defaults, key
   bindings, recipes, editor commands, and anything someone might want
   different, are fuxth.
8. **Text stays data.** Callbacks, bindings and hooks hold code values or
   word names, never strings that become code. fux never runs a string as
   source.
9. **Self-describing.** Every word carries its documentation, every key can
   say what it runs, and every recipe in the agent guide runs in CI.
10. **Independent programs.** fux, the editor and fuxth each build and run
    alone. fux doesn't know the editor exists, the editor doesn't know it
    runs in fux, and fuxth knows neither. Integration is the agent's job,
    through their interfaces. A project contains no code specific to another
    tool.
11. **Finished parts stay finished.** A part marked frozen changes only for
    a bug or an owner decision, never because an audit had an idea.

## What it is not

- **Not an agent framework.** No model calls, scheduler, task queue or
  message bus inside fux or fuxth. Agents coordinate through files and
  processes outside them.
- **Not a security sandbox.** Scope prevents accidents between cooperating
  agents; the operating system isolates hostile ones.
- **Not a judge of readiness,** ever.
- **Not a clone** of tmux, Emacs or Forth. It takes Emacs's architecture
  (command loop, keymaps, hooks, self-documentation, live redefinition) and
  Forth's mechanisms (the dictionary, early binding, an extensible
  compiler), not their syntax, keys or file formats.
- **Not an IDE.** No language servers or syntax trees in the editor until a
  finding asks; analysis runs as programs in panes.
- **Not mouse-first, no floating panes** (rejected 2026-10-26).
- **Not a general runtime.** No threads, floats, raw memory, packages or
  network in fuxth.
- **Not a plugin platform.** Extension is fuxth source anyone can read.
- **Not finished by audits.** An audit can find a bug; only a working
  scenario says "good enough".
- **Not stable inside.** The frame protocol and the Rust internals change
  whenever that helps. Only the agent interfaces and frozen parts promise
  anything.

## How anyone can tell it's working

1. **The scenario suite passes** (`ROADMAP.md`): things an agent must be
   able to do, each with an end state a script checks, from a clean
   machine.
2. **The weekly report,** built from the agents' own logs, shows:
   - calls per finished task and bytes read per call, flat or falling;
   - failures by error code, and the calls each recovery took;
   - failures after effects: counted, each with its effect list;
   - acts outside scope: some refused, none succeeded;
   - tasks where an agent gave up on the tools and used something else,
     falling.
3. **Most work is real work.** At least 60% of the agents' active time goes
   to work beyond improving fux, the editor and fuxth.
4. **The bounds hold under load.** Ten agents and their builds run for a
   week with no OOM kill and no fux crash.
5. **The owner's load is one page a day.** No question ever reaches the
   owner; the decision panel settles them, and nothing waits on a person.
6. **Decisions stick.** Reversals are near zero a month, and each cites new
   evidence from use, never an audit's opinion.
7. **The core stops moving.** The things table, the fuxth core words and the
   semantics page stay unchanged for 30 days while the library grows.
8. **The machine numbers hold.**

| Property | Today | Promise |
|---|---|---|
| Keystroke echo, idle / beside a flood (p50) | 0.24 / 0.25 ms | ≤ 0.5 / ≤ 1 ms |
| Plain text to the screen | 151 MB/s | ≥ 100 MB/s |
| Idle wakeups in 10 s | 0 | 0 |
| Server memory at start / per empty pane | 5.3 MiB / 60 KiB | ≤ 8 MiB / ≤ 128 KiB |
| One `fux` command, cold | 8.25 M instructions | ≤ 10 M |
| fuxth steps a second | 130–140 M | ≥ 100 M |
| Longest hold of the server loop by one request | not gated | ≤ 16 ms |
| Peak heap of a fuxth run | not gated | its byte budget + 1 MiB |
