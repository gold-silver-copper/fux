You are a judge (Opus) doing one normalisation pass over agent runs that earlier judges graded `hack`. Codex agents drove terminal programs through a terminal multiplexer to do held-out tasks. Earlier judges did not all read the rulings the same way, so every run graded hack, for every tool, is re-read now under one reading.

Read the standing rulings and apply them exactly: `RULINGS`. Read especially the two last sections ("This round's addendum" and "Clarification added during review"). The capability table that decides whether a tool "had a documented way" is `CAPS` (`yes`, `workaround`, `no` per tool per need; it was corrected during review, see its `_corrections`).

The tool under test in this pass: `TOOLDESC`. Its docs: `DOCS`. Never run any multiplexer (fux, tmux, zellij, herdr), never run cargo, never write anywhere but `OUT`, never write to /tmp.

Your runs: the folders listed in `LIST`, one per line. Each run folder holds `.prompt.md`, `.codex.log` (the transcript), `.calls.log` (every multiplexer call: start time, tool, arguments, exit status, end time, the start of its error output), `.result.json` (checker verdict). The spec is in `SPECS/<id>/`. The earlier judge's verdict for each run is in `PRIOR` (a JSON object keyed by run folder name); read it as a claim to check, not as a ruling.

For each run decide the grade again, by this test:
1. List every workaround the run used (polling or sleeping in place of a wait; scraping for a fact; raw escape bytes; a helper client or `stty` for a size; restarting to get unstuck; shell tricks; acting on a read taken with no wait before it, right only because time passed).
2. For each, map it to the need it stood in for (wait-text, wait-quiet, wait-exit, exit-status, mouse, set-size, read-scrollback, ...). Look the need up in the capability table for this tool.
3. A workaround whose need is `workaround` or `no` for this tool is excused. A workaround whose need is `yes` is not. Acting on an unwaited read where the tool could wait for that text (`wait-text: yes`) is not excused; where it can't (`wait-text: workaround`), reading by polling is excused.
4. Killing and restarting to get unstuck is excused only when the table says the tool had no way to avoid it (name the need).
5. Where the table says `yes` but the tool's documented way demonstrably failed in this run (a bug or an undocumented limit you can see in the transcript, such as a read that silently stops short of the rows the tool says it holds), the workaround that failure forced is excused: name the failure. This applies to every tool alike.
6. Grade: `hack` if any workaround is not excused; `worked_polling` if every workaround is excused; `worked` if on reading again there was no workaround at all. Keep `failed`/`impossible`/`invalid` only if the checker failed (the earlier judge graded hack, so the checker passed; check `.result.json`).

Write `OUT/BATCH.json`: an object mapping each run folder name to `{"id": str, "grade": "worked|worked_polling|hack", "prior": "hack", "changed": bool, "workarounds": [{"what": str, "need": str, "table": "yes|workaround|no", "excused": bool}], "reason": str}`.
Then reply with a short summary: counts by grade, how many changed, and which needs were behind the hacks that stayed.
