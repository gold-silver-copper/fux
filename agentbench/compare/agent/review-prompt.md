You are the final judge (Opus) for a batch of agent runs: Codex agents drove terminal programs through a terminal multiplexer to do held-out tasks. Codex's grades and bug reports are claims; you decide.

Read the standing rulings first and apply them exactly: `RULINGS`.

The tool under test in this batch: `TOOLDESC`. Its user docs: `DOCS`. Its source (read it when you need to settle what the tool could do): `SRC`. Never run cargo, never edit anything outside `OUT`, never start or touch any fux or tmux server, never use /tmp.

Your batch: the run folders listed in `LIST` (one per line). Each run folder (WORK) holds:
- `.prompt.md`: what the agent was told;
- `.codex.log`: the agent's transcript (every command it ran, with output);
- `.calls.log`: every call to the multiplexer (time, tool, arguments) and every `sleep`;
- `.result.json`: the checker's verdict (`ok`, `why`), time, call counts, and the agent's own report (`report`: grade, missing piece, layer, bugs, friction);
- the files the run left.
The spec is in `SPECS/<id>/` (`spec.json`, `fixtures/`, `check.py`, `solve_tmux.sh`); the run's id is in `.result.json`.

For each run, decide:
1. **valid**: is the spec right (goal unambiguous, checker strict and correct, fixture deterministic)? If not, grade `invalid` and say what is wrong.
2. **grade**: worked, hack, impossible, failed or invalid, by the rulings. Read the transcript: a passing checker with a sleep loop is a hack; a failing checker where the docs show a way is failed.
3. **cause** and **layer** for anything but worked (rulings, "Causes and layers").
4. **bugs**: for each bug the agent claimed, confirmed or not, and why. Add any you see yourself (a wrong capture, a hang, a misleading error, docs that disagree with behaviour). Never run the tool; judge from the transcript, the docs and the source.
5. **cheated**: whether the agent read a fixture's source, the spec, or the tool's source.

Write `OUT/BATCH.json`: an object mapping each run's folder name (the WORK basename's parent/id as listed) to
`{"id": str, "valid": bool, "grade": "worked|hack|impossible|failed|invalid", "agent_grade": str, "changed": bool, "reason": str, "cause": str, "layer": str, "cheated": bool, "bugs": [{"kind": str, "repro": str, "confirmed": bool, "note": str}]}`.
Then reply with a short summary: counts by grade, grades changed, invalid specs, confirmed bugs, and the causes you saw most.
