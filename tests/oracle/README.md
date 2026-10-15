# The oracle

`tests/oracle.rs` checks fux's answers against what a program says it did.
The program is `fixture.py` (Python 3, stdlib only): it writes a side log of
every byte it writes to its terminal, whether that byte is the echo of what
was typed or its own output, what it read, and when it blocked. The checker
drives it through fux as an agent does (`run`, `send-keys` with `wait-text`
and `wait-bell`, `wait`, `resize`), reads the side log once the case ends,
and decides what each answer should have been:

- a text wait is met (status 0) iff some `write` with `role: output`, after
  the program read the first byte of the action's input and before fux
  answered, holds the text (as fux finds a text: case ignored, from a word's
  start). Text drawn over counts: it was printed. Output written after fux
  wrote the input but before the program read it may or may not count, and
  is not judged;
- it is never met by text only in `role: echo` writes, or in the kernel's
  echo (line mode: bytes no write covers);
- of a text and a bell, the one written first is met;
- `reading` (and status 7) only while the side log shows the program in
  `block on read` with every key it can read read (in line mode: up to the
  last Enter). A plain `wait` during which the program sat in such a read,
  nothing written, must answer `reading`. Linux only: elsewhere fux is not
  told what a program is blocked in.

Every disagreement is reported under a class, with the `classify` source
that applied (`kernel`: line mode; `order`: the program edits its own line
in raw mode; `screen`: the alternate screen; no fixture is a shell, so
never `marks`):

| class | fux | the side log |
|---|---|---|
| `echo-taken-as-output` | met the text | it was only echoed |
| `output-taken-as-echo` | did not meet it | printed, on a line with what was typed |
| `missed-overwritten` | did not meet it | printed, then drawn over |
| `ended-early` | 7: cannot come | printed |
| `missed-output` | 124 | printed |
| `stale-output`, `met-without-text` | met | printed before the input, or never |
| `wrong-order` | met the bell (the text) | the text (the bell) came first |
| `wrong-state` | `reading`/7 | not blocked reading every key (or the reverse, for a plain `wait`) |
| `fixture-exited` | the program exited | (a fixture crash) |

Disagreements known to be fux's are listed in one place, `KNOWN` in
`tests/oracle.rs`, each with why; the deterministic tests report them and
pass. Remove an entry once fixed.

## Running

```sh
cargo test --test oracle -- --test-threads=4            # the deterministic part
ORACLE_SEED=7 ORACLE_SECS=240 ORACLE_JOBS=4 \
  cargo test --test oracle -- --ignored                 # fuzzing
```

Fuzzing generates random cases from `ORACLE_SEED` (at most `ORACLE_RUNS`,
within `ORACLE_SECS`), each with a random subset of behaviours (swarm
testing), a random sequence of actions (type-ahead, resizes in the middle
of a burst, screen switches), a random size and chunking. A failing case is
shrunk (delta debugging over its actions, then its keys) and saved to
`corpus/<class>-<n>.json`, which the deterministic part replays.
`ORACLE_SAVE_KNOWN=1` saves known disagreements too; `ORACLE_KEEP=DIR` keeps
every side log with fux's answers.

## The fixture

`fixture.py --mode line|password|readline|ptk|full --seed N [--chunk N]`;
its docstring lists the commands a line may hold. Two choices the format
leaves open: a read with the terminal's own time limit (VMIN 0, VTIME) is
logged as `block on poll`, since it returns alone; and with `--chunk N`
each piece of a write is its own `write` record.

## Side-log format (frozen)

[side-log-format.md](side-log-format.md), copied unchanged from the notes
(`notes/projects/fux/fux-provenance/side-log-format.md`). The fixture writes
it and the checker reads it as written.
