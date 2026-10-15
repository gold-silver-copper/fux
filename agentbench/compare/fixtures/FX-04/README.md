# FX-04

Raw prompt-toolkit-like three-row input-area redraw, completion menu, Tab completion, editing, and bells.

Run with `python3 FX-04/fixture.py --seed 0` from the suite directory,
or `python3 fixture.py --seed 0` from this folder. Both stdin and stdout
must be terminals. Python 3 and its standard library are the only dependencies;
no subprocesses or network operations are performed by the fixture.

Arguments: `--seed N` (integer, default 0). Other options are defined as:

- `'--lines', type=int, default=1`

Ground truth: The initial three-row area is output. Every later whole-area redraw after a key is echo, including the completion list and help row. Submission moves below the help row in an echo write, then emits an echo CRLF; Executing: is output. Each redraw paints all three rows and returns the cursor to the insertion point in the input row. This is a small prompt-toolkit-like emulator using only the standard library. Editing is ASCII only.

All delays and generated content use a local `random.Random(seed)` or explicit
VMIN/VTIME arguments; thread timing necessarily also depends on OS scheduling.
`FUX_ORACLE_LOG` enables the frozen JSONL side log; when unset there is no log.
Each terminal write is serialized with its record. Output translation is disabled
before the first prompt, so the logged bytes are exactly the emitted PTY bytes.
The initial inherited mode and subsequent mode changes are logged, and the saved
terminal settings are restored and logged before the final exit record.
No terminal flushing discards queued input. Canonical input maps Enter CR to LF.

The shared `../oracle_support.py` implements logging, termios changes and the
small editors. The shared `../pty_selftest.py` implements PTY driving and byte
validation. Each fixture depends only on these files and its own folder.
Run `python3 selftest.py` here (or `python3 FX-04/selftest.py` at the suite root).
It runs every scenario, drains all PTY bytes without interpreting screen content,
and checks the log, write/echo accounting, modes, read totals, answer offsets,
thread names, BEL accounting, and clean exit. Scenario pauses are minimum waits;
startup is synchronized to the first blocking point. All included scenarios exit.
