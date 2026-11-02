# fux: the vision

Approved by the owner on 2026-10-31 (D-64 to D-66, D-81 to D-85). It is grounded in a
terminal multiplexer ontology
(`~/code/notes/projects/fux/ontology-2026-10-31/ontologist/ONTOLOGY.md`)
and a gap check of master against it
(`~/code/notes/projects/fux/ontology-2026-10-31/gaps/GAPS.md`). This page
changes only by an owner decision recorded in `DECISIONS.md`.

## The goal

**fux is a minimal terminal multiplexer, driven and configured entirely in
fuxth, a small Forth.** An agent driving it can:
- read every fact the ontology defines;
- learn every multiplexer and program change from the journal, which is
  bounded;
- see screen changes through versions and captures;
- get OS facts, such as a program's working directory, as live
  observations.

Every action acts on what was seen, says what it did, and never guesses.

## Who it is for

1. **Agents first.** Claude, Codex and whatever comes next, driving fux one
   command or one small fuxth program at a time. The interface is designed
   for a reader who pays for every byte and whose next turn starts with a
   fresh shell.
2. **The person who watches and steers.** The owner attaches to watch the
   agents' work and steps in when they choose. The owner doesn't have to
   live in fux for it to succeed.
3. **People who want to live in it.** Welcome, and the same words serve
   them. A feature only they need (themes, mouse menus, a richer bar) waits
   until someone asks for it.

"Agents first" never means code that knows about a particular agent. There
is one interface.

## The things that exist

This list is the anchor. **Adding, removing, splitting, merging or renaming
a row is a vision change** and needs an owner decision. Each thing's facts,
the events and the derived facts are in the ontology, and that is the
reference.

| Thing | What it is | Identity | Lifetime |
|---|---|---|---|
| server | owns everything for one run | its socket and its run | start until told to stop |
| workspace | an ordered group of tabs and workspaces, seen one at a time (tmux: session) | `+N` plus run; a name or none | until its last child closes |
| tab | panes laid out together, split side by side or stacked one at a time, seen one at a time (tmux: window) | `@N` plus run; a name or none | until its last pane closes |
| pane | a terminal that outlives its programs | `%N` plus run; a name or none | until killed |
| program | one run of a process in a pane; a respawn is the next generation | pane plus generation | start to exit |
| client | an attached terminal, or a headless stand-in | `cN` plus run | attach to detach |
| journal | the numbered, bounded events of one run | the run | the run's |
| dictionary | the words one fuxth run can call | one per run | the run's |
| word | a named fuxth definition | its name plus which definition | its dictionary's |

**Not things, on purpose:**
- **judgements:** readiness, idle, done;
- **an agent:** it's a program in a pane;
- **a command:** it's a word;
- **a setting:** it's a value;
- **a hook:** it's a program reading the journal;
- **a record or capture:** it's a value holding facts at one version;
- **a version:** it's a derived fact;
- **a layout:** it's a fact of its tab;
- **a timestamp;**
- **a scope or sandbox** (D-81).

## Principles that never change

1. **Facts, never judgements.** fux reports what happened: a version moved,
   a program exited with 7, a text is on the screen. It never decides that
   a program is ready, idle or done. Nothing is guessed: no made-up names,
   values, defaults or spelling suggestions.
2. **Act on what you saw.** An action can carry the version it was based
   on, and is refused as stale if the thing moved.
3. **Every failure says what already happened.** A fuxth program can fail
   after it acted, so its error lists the actions that already took effect.
4. **Limits belong to users and the OS** (D-83). fux sets no limits of
   its own on memory, steps, time or queues:
   - **the OS manages those:** `ulimit`, cgroups, `timeout`, `systemd-run`;
   - **the user sets sizes** where fux must keep something, such as
     history. Those are preferences with defaults, readable like any other
     fact.

   The one exception is parsing untrusted wire input safely. When something
   does fail, the error says so with a code.
5. **Deterministic, with its inputs stated.** The same source, the same
   starting state and the same host answers give the same result. The order
   in which competing agents arrive is an input, not something fux makes
   repeatable.
6. **One language, mechanism in Rust, policy in fuxth.** Atomic changes,
   bounded observations, the terminal engine and OS facts are Rust.
   Defaults, key bindings, recipes, and anything someone might want
   different, are fuxth.
7. **Text stays data.** Callbacks, bindings and hooks hold code values or
   word names, never strings that become code. fux never runs a string as
   source.
8. **Self-describing.** Every fact in the ontology can be read with a word,
   every word carries its documentation and can be listed from the
   dictionary, and every key can say what it runs.
9. **Independent programs.** fux, fuxth and fuxed each build and run alone.
   fux doesn't know the editor exists, and fuxth knows neither. A project
   contains no code specific to another tool.
10. **The Unix philosophy** (D-82, D-91, D-92). fux, fuxth and fuxed each
    do one thing well, and work with other programs instead of growing.
    - **Lean on the OS.** What the OS or an ordinary program already does,
      they use or report, never rebuild:
      - processes, signals and exit statuses;
      - files, permissions and the environment;
      - users, isolation and limits;
      - logging and scheduling;
      - clipboards, paging and search.

      Every feature and review asks whether the OS or an ordinary program
      already does it.
    - **Text streams.** stdout carries only data: an item per line, or a
      record as one JSON object per line. Diagnostics go to stderr, and the
      exit status says how it ended. Nothing that runs without a terminal
      prompts or chats.
    - **Silence.** A command with nothing to say prints nothing.
    - **Visible state.** Whatever they keep that a person may want to read
      is readable with ordinary tools.
    - **Policy outside the mechanism.** Optional recipes are files a user
      loads, not part of every program.

    They compose through arguments, stdin, stdout, stderr, exit codes and
    files.

    **The boundary:** the shell composes `fux` commands, fuxth does what
    must happen inside one request or one key binding, and ordinary
    programs do everything else.
11. **Finished parts stay finished.** A part marked frozen changes only for
    a bug or an owner decision, never because an audit had an idea.

## What it is not

- **Not a sandbox, and it has no confinement** (D-81). Any client can act
  on any part of its server. Agents that must not touch each other's work
  use separate servers or OS isolation. Confinement built into fux would be
  easy to break and would promise a safety it can't give.
- **Not an agent framework.** No model calls, scheduler, task queue or
  message bus inside fux or fuxth. Agents coordinate through files and
  processes outside them.
- **Not a judge of readiness,** ever.
- **Not an editor.** fuxed is its own project, with its own vision.
- **Not a clone** of tmux or Forth. fuxth works like a real Forth: a
  dictionary, an outer interpreter, immediate and defining words, a data
  space, execution tokens (D-84). Its words are full English words that need
  no quoting in a shell, such as `define … end`, `add` and `str`, never
  Forth's symbols such as `:` `;` `+` and `s"` (D-87).
- **Not mouse-first, no floating panes** (rejected 2026-10-26).
- **Not a general runtime.** No threads, floats, packages or network in
  fuxth. It is otherwise a real Forth (D-84). Its data space holds
  memory-safe cells, never host memory.
- **Not a plugin platform.** Extension is fuxth source anyone can read.
- **Not finished by audits.** An audit can find a bug; only the checklist
  says "done".
- **Not stable inside.** The frame protocol and the Rust internals change
  whenever that helps. Only the agent interface and frozen parts promise
  anything.

## When it is done: fux 1.0 (D-65)

fux 1.0 is reached when all of these hold:
- **every row of the gap check is "have":** the gap check as amended by
  panel sitting 5 and D-81;
- **the adopted removals are done** (D-68);
- **every changed contract has been shown working in the real program** on
  the release head (D-48);
- **the local gate passes;**
- **fuxth's Forth core has landed** (D-84), since fux's words are fuxth
  words;
- **fux is smaller than it was on 2026-10-31** (D-85): its production
  lines, not counting fux-vt and fuxix, end below master `1a8f860f`.

Bounded journal batches read by cursor, with `missed` counts, count as
complete.

Reaching it is a checkpoint, not a freeze (D-118): fux stays in active
development. When the owner declares the freeze, fux's public words, record
fields, events and error codes freeze; bug fixes, library definitions and
internals stay free, and only a new thing, fact or event needs the owner.

The machine numbers hold throughout:

| Property | Today | Promise |
|---|---|---|
| Keystroke echo, idle / beside a flood (p50) | 0.24 / 0.25 ms | ≤ 0.5 / ≤ 1 ms |
| Plain text to the screen | 151 MB/s | ≥ 100 MB/s |
| Idle wakeups in 10 s | 0 | 0 |
| Server memory at start / per empty pane | 5.3 MiB / 60 KiB | ≤ 8 MiB / ≤ 128 KiB |
| One `fux` command, cold | 8.25 M instructions | ≤ 10 M |
| fuxth steps a second | 130–140 M | ≥ 100 M |
| Longest hold of the server loop by one request | not gated | ≤ 16 ms |
