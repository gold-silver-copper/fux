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
     - the language's step and byte budgets;
     - fux's own caps and admission limits.
   - **Kept:** only the sizes a user sets as preferences (history, the
     journal ring), and what parsing untrusted wire input safely needs.
   - **Not touched:** fux-vt and fuxix, which are frozen (D-42 still holds
     for them).
   - It runs alone, because it touches every crate.
2. **grift replaces fuxth.** fux's language is grift, a Lisp of its own project, and fuxth, its
   runner and fuxed are kept in `archived/`. The plan that would have made fuxth a real Forth
   (D-84), and the fuxed experiment on it, end there.

## Then

- **The fux 1.0 list** (D-80):
  - the gap check's tasks, minus task 31 (confinement, D-81), task 9 (the
    limits record, D-83) and task 25 (step and byte usage, D-83);
  - the removals (D-68);
  - `since` and `faint-marks`.

  Its language lane (code values, word records, failure reports) goes with
  grift. Page: `~/code/notes/projects/fux/live/milestones/1.0.md`.

## fux 1.0 (D-65, D-84, D-85)

It is reached when all of these hold:
- every gap-check row is "have";
- the removals are done;
- grift has replaced fuxth;
- each changed contract has been shown in the real program;
- fux is smaller than on 2026-10-31;
- the gate passes.

Then fux's public interface freezes, as VISION.md says.

## Not on this roadmap

- Confinement, scope, or any sandboxing (D-81).
- Limits fux imposes on its own (D-83).
- A human-facing polish milestone (themes, reload, mouse). It waits for an
  owner request.
