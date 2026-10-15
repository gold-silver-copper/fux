# FX-06

Noncanonical timed reads with VMIN/VTIME: empty timeouts, interbyte timeout, and byte-count threshold.

Run with `python3 FX-06/fixture.py --seed 0` from the suite directory,
or `python3 fixture.py --seed 0` from this folder. Both stdin and stdout
must be terminals. Python 3 and its standard library are the only dependencies;
no subprocesses or network operations are performed by the fixture.

Arguments: `--seed N` (integer, default 0). Other options are defined as:

- `'--reads', type=int, default=3`
- `'--vmin', type=int, choices=[0, 3], default=0`
- `'--vtime', type=int, choices=[1, 2, 3], default=None`

Ground truth: The kernel never echoes. Received packet bytes are echoed by the program in a separate echo write. Packet diagnostics are output answering the read offset. An empty timeout logs read with empty b64 and unchanged consumed; its diagnostic has answers null. With VMIN=3 and VTIME>0, the timer starts only after the first byte, so an initial read can wait indefinitely. Reads have a 32-byte maximum and can consume more than VMIN when queued input is available.

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
Run `python3 selftest.py` here (or `python3 FX-06/selftest.py` at the suite root).
It runs every scenario, drains all PTY bytes without interpreting screen content,
and checks the log, write/echo accounting, modes, read totals, answer offsets,
thread names, BEL accounting, and clean exit. Scenario pauses are minimum waits;
startup is synchronized to the first blocking point. All included scenarios exit.
