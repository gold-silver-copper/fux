# FX-05

Alternate-screen full-screen dashboard; raw navigation, note echo, cursor addressing, colors, and bells.

Run with `python3 FX-05/fixture.py --seed 0` from the suite directory,
or `python3 fixture.py --seed 0` from this folder. Both stdin and stdout
must be terminals. Python 3 and its standard library are the only dependencies;
no subprocesses or network operations are performed by the fixture.

Arguments: `--seed N` (integer, default 0). Other options are defined as:

No other arguments.

Ground truth: Screen navigation, selected-row redraws, and inspection results are output answering the key, while redraws of the typed note are echo. Enter inspects the selected job and is not line submission. Alternate-screen entry/exit bytes are output; the mode event follows the control write that effects the transition. The cursor is shown again before leaving.

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
Run `python3 selftest.py` here (or `python3 FX-05/selftest.py` at the suite root).
It runs every scenario, drains all PTY bytes without interpreting screen content,
and checks the log, write/echo accounting, modes, read totals, answer offsets,
thread names, BEL accounting, and clean exit. Scenario pauses are minimum waits;
startup is synchronized to the first blocking point. All included scenarios exit.
