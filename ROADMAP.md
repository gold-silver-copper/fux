# fux: the roadmap

Approved by the owner on 2026-10-31 (D-64 to D-66, D-79 to D-81). The
road to 1.0 is a closed list, not a series of scenarios that keep growing.
Every task is one PR, written by Opus (D-40) and reviewed by Sol and Astra
(D-63). Each is verified with its own build in the real program (D-48) and
merged on the local gate (D-39).

## Done

- **M0, stop and unblock:**
  - adopted `DECISIONS.md` and the review rule;
  - ended the simplification pass (D-41).
- **M0c, a finite interface** (#321–#332):
  - three crates;
  - generated lists of requests, words, records and codes;
  - a newer client follows an older server;
  - cassettes;
  - `run` removed;
  - `confirm` built on `choose`.

## M0b, cut down (D-79): running

Three tasks are merged: `region` #333, `fuxth-region` #334 and
`request-region` #336. Two remain:
- **`pane-budget`:** it includes connection and pane admission (K10).
- **`client-region`.**

Each proves its bound with a flood beside an interactive pane and a blocked
or disconnecting client. The small `Session::check()` invariants also go in,
under the existing `checks` feature. Page:
`~/code/notes/projects/fux/live/milestones/M0b.md`.

## The 1.0 list (D-80): next

One list, ordered by dependencies first and size second, worked in disjoint
lanes. Page: `~/code/notes/projects/fux/live/milestones/1.0.md`. It
contains:
- **the gap check's tasks** (`GAPS.md`, "Candidate tasks"), minus task 31
  (confinement, D-81);
- **the removals** (D-68);
- **`since` and `faint-marks`,** as facts of the terminal state;
- **the fuxth support those tasks need:** code values for callbacks (D-68),
  word and dictionary records (D-75) and failure reports;
- **last, `agent-page`:** the agent guide rewritten around the finished
  interface.

The hidden bar (D-49) is in it as gap task 4.

## fux 1.0 (D-65)

Every gap-check row is "have", the removals are done, each changed
contract is shown in the real program, and the gate passes. Then the public
interface freezes, as VISION.md says.

## After 1.0, as separate projects

- **fuxth on its own** (from the old M2): the dictionary and outer
  interpreter, `SEMANTICS.md`, the `fuxth` runner and REPL.
  `/home/gsc/code/fux/prompts/fuxth-forth.md` as cut by D-36.
- **fuxed, the editor** (from the old M3), with its own vision: named
  commands, live redefinition, edits checked against a buffer revision.
- **Using the tools** (from the old M4): the owner's agents working in fux
  every day. It is no longer a condition for 1.0. What they find becomes
  bug fixes, or an ontology change for the owner to decide.

## Not on this roadmap

- Confinement, scope, or any sandboxing (D-81).
- A human-facing polish milestone (themes, reload, mouse). It waits for an
  owner request.
