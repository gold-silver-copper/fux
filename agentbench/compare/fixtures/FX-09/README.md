# FX-09

Busy sleeping/computing with canonical echo; typed-ahead input; carriage return, cursor move, and erase within one write.

Run with `python3 FX-09/fixture.py --seed 0` from the suite directory,
or `python3 fixture.py --seed 0` from this folder. Both stdin and stdout
must be terminals. Python 3 and its standard library are the only dependencies;
no subprocesses or network operations are performed by the fixture.

Arguments: `--seed N` (integer, default 0). Other options are defined as:

- `'--jobs', type=int, default=2`
- `'--compute', action='store_true'`

Ground truth: The kernel echoes keys even while the program is busy. They are read later, so replies use consumed bytes, never merely the count of bytes typed. The busy scan status has answers null. Each queued reply is one output write containing a transient scanning line, carriage return, erase, cursor move, and final text. --compute uses a seeded iteration count, while the default uses a seeded sleep.

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
Run `python3 selftest.py` here (or `python3 FX-09/selftest.py` at the suite root).
It runs every scenario, drains all PTY bytes without interpreting screen content,
and checks the log, write/echo accounting, modes, read totals, answer offsets,
thread names, BEL accounting, and clean exit. Scenario pauses are minimum waits;
startup is synchronized to the first blocking point. All included scenarios exit.
