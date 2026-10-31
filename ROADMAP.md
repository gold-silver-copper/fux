# fux: the roadmap

Approved by the owner on 2026-10-31 (D-64 to D-66, D-79 to D-85). The road
to 1.0 is a closed list, not a series of scenarios that keep growing.
- Every task is one PR, written by Opus (D-40) and reviewed by Sol and
  Astra (D-63).
- Reviews keep fux minimal (D-85): must-fixes with evidence, at most three
  notes, and no new surface.
- Each task is verified with its own build in the real program (D-48) and
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
- **M0b** was cancelled by D-83 after #333, #334 and #336. Limits belong to
  users and the OS.

## Now

1. **`unlimit`** (D-83): one removal PR.
   - **Removed:**
     - the region and budget machinery (#333, #334, #336);
     - fuxth's step and byte budgets;
     - fux's own caps and admission limits.
   - **Kept:** only the sizes a user sets as preferences (history, the
     journal ring), and what parsing untrusted wire input safely needs.
   - **Not touched:** fux-vt and fuxix, which are frozen (D-42 still holds
     for them).
   - It runs alone, because it touches every crate.
2. **The fuxth design** (D-84), beside `unlimit` (no code): the staged plan
   that turns fuxth into a real Forth. It covers:
   - Forth 2012 Core, with each deviation and its reason;
   - the standalone `fuxth` runner with the terminal and file words an
     editor needs (`key`, `emit`, `type`, `at-xy`, `page`, the file word
     set), leaning on the OS (D-82);
   - the spec of fuxed version 0.

   It revises `/home/gsc/code/fux/prompts/fuxth-forth.md` and its page is
   `~/code/notes/projects/fux/live/milestones/fuxth.md`.

## Then, in parallel (two lanes at a time)

- **The fuxth track:** the plan's PRs, in the fuxth crates.
- **The fuxed experiment,** as soon as the runner can read keys, draw and
  open files.
  - **Where:** a completely independent fuxth program, which doesn't use
    fux, in `fuxed/`.
  - **Who:** an Opus agent writes it, and records every missing word or
    mechanism as a finding.
  - **Findings go into the fuxth track.**
- **The fux 1.0 list** (D-80):
  - the gap check's tasks, minus task 31 (confinement, D-81), task 9 (the
    limits record, D-83) and task 25 (step and byte usage, D-83);
  - the removals (D-68);
  - `since` and `faint-marks`.

  Its fuxth lane (code values, word records, failure reports) moves to the
  fuxth track. Page: `~/code/notes/projects/fux/live/milestones/1.0.md`.

## fux 1.0 (D-65, D-84, D-85)

It is reached when all of these hold:
- every gap-check row is "have";
- the removals are done;
- fuxth's Forth core has landed;
- each changed contract has been shown in the real program;
- fux is smaller than on 2026-10-31;
- the gate passes.

Then fux's public interface freezes, as VISION.md says. fuxth and fuxed
keep going toward their own 1.0.

## Not on this roadmap

- Confinement, scope, or any sandboxing (D-81).
- Limits fux imposes on its own (D-83).
- A human-facing polish milestone (themes, reload, mouse). It waits for an
  owner request.
