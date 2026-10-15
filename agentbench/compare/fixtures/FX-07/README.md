# FX-07

Input polling with select timeouts; redraw while typing; bells alone and in a write with text.

Run with `python3 FX-07/fixture.py --seed 0` from the suite directory,
or `python3 fixture.py --seed 0` from this folder. Both stdin and stdout
must be terminals. Python 3 and its standard library are the only dependencies;
no subprocesses or network operations are performed by the fixture.

Arguments: `--seed N` (integer, default 0). Other options are defined as:

- `'--commands', type=int, default=1`
- `'--idle-ticks', type=int, default=12`

Ground truth: All select waits log block on poll. Reads are performed only after readiness and log block on read. Typed characters and buffer redraws are echo; submission CRLF is echo. Timeout notifications and all BEL bytes are output with answers null for timer events. A timer-triggered redraw of the existing typed line is echo with answers null. Reactions to submitted commands answer the current consumed offset.

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
Run `python3 selftest.py` here (or `python3 FX-07/selftest.py` at the suite root).
It runs every scenario, drains all PTY bytes without interpreting screen content,
and checks the log, write/echo accounting, modes, read totals, answer offsets,
thread names, BEL accounting, and clean exit. Scenario pauses are minimum waits;
startup is synchronized to the first blocking point. All included scenarios exit.
