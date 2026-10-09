# fux-walk: seeded walks over a real fux

An excluded workspace with its own lockfile. It drives a real `fux` binary
(a fresh server, real `fux attach` clients on PTYs, and the command line)
through seeded random steps, checks every invariant after every step,
saves each walk as a trace, and minimizes a failing walk to the steps that
matter.

Besides fux, fux-vt and fuxix by path it uses `serde_json` (to read `ls
--json` and `capture-client --json`) and `signal-hook` (to stop cleanly
between steps on SIGINT or SIGTERM). It forbids fux's lints. CI runs its
fmt, clippy and `cargo test`, but no walks.

## Commands

From the repository root:

```sh
cargo build --locked
cargo build --manifest-path walk/Cargo.toml

walk/target/debug/fux-walk --fux target/debug/fux --seed 1 --seeds 20 --steps 300  # 20 walks, seeds 1-20
walk/target/debug/fux-walk --fux target/debug/fux --seed 777 --steps 3000           # one long walk
walk/target/debug/fux-walk --fux target/debug/fux --replay walk/runs/RUN/trace.txt
walk/target/debug/fux-walk --fux target/debug/fux --replay-all DIR                  # every trace in DIR
```

| Flag | Meaning |
| --- | --- |
| `--fux PATH` | required; made absolute first. The walk never builds fux or looks it up on `PATH` |
| `--seed N`, `--seeds N`, `--steps N` | first seed (1), number of walks (1), steps per walk (300) |
| `--seconds N` | bound on the whole run (3600) |
| `--settle MS` | how long a step may take to settle (5000; raise it under emulation) |
| `--minimize-seconds N` | bound on minimizing (900) |
| `--output DIR` | where runs are saved (`walk/runs/`, ignored by git) |

Exit status 1 means a walk or replay failed, or the run was interrupted.
Walk a debug build of fux: it checks arithmetic for overflow.

## The fixture

Each walk and replay starts a fresh `fux server` in a new 0700 directory
under `/tmp` (macOS's `$TMPDIR` is too long for a socket path), with a
config file the walk may rewrite (`set shell /bin/sh`), an environment
cleared but for `PATH`, `HOME` set to the directory, and `act.sh`, the
script panes act through. The first client is a `fux attach` at 24×80,
whose screen the walk parses with fux-vt.

Only what the walk started is ever signalled, every wait is bounded, and
cleanup runs however a walk ends. A pane process that outlives the server
is a failure.

## Steps

Each step is a small random index resolved against the live `ls --json`,
recorded as the concrete step it became:

- **keys:** default bindings, layers and repeat modes; overlays and copy
  mode driven by their keys; command-prompt lines, some wrong; a lone Esc,
  the doubled prefix, unbound, capital and Ctrl letters; a bracketed paste;
  plain typing; `C-b d`;
- **commands** from the whole set, with valid and invalid targets (never
  `kill-server`);
- **what no command expresses:** a client resized (down to 1×1, 1×200,
  40×1); a client asking for 9000×9000, which must get 4096×4096; a pane's
  program acting through `act.sh` (alternate screen, mouse reporting,
  bracketed paste, application cursor keys, `stty`, a burst of output, not
  reading, exiting with a status); SIGSTOP and SIGCONT to a pane's shell
  (every stopped pane is continued within a few steps); SIGHUP to a client;
  the config file rewritten, then `reload`; one to three clients attaching
  and leaving.

While fewer than two panes are left, the walk splits one first. A key path
may still close the last pane and with it the server: that walk ends
there, counted as ended, not failed.

## Settling and invariants

After each step the walk waits until it has settled, never with a bare
sleep: an act's marker shows in its pane, and every client's painted
screen equals its `capture-client`. Then:

- **the server** runs, answers, and its log has no panic;
- **structure** (`ls --json`): ids are unique (a client's workspace, tab
  and focused pane belong together by fux's types);
- **processes:** a pane's shell ends within three seconds of its pane
  closing;
- **sizes:** a pane a client surely shows fits that client's pane area;
- **the frame oracle:** every client's terminal shows, row for row, what
  the server composes for it (`capture-client`);
- **painting:** the screen has the client's rows, none wider than its
  columns, no wide glyph starting in the last column; the bar names the
  client's workspace and tab where there is room, and says `[zoom]` when
  zoomed and nothing else claims its right side;
- **overlays:** a step that is not keys or a command opens none;
- **error notices:** every notice in fux's error colour matches a template
  taken from fux's source (`walk/src/notices.rs`: every string literal,
  each `{…}` a wildcard), never a hand-kept list.

Narrowed, and why:

- **Sizes** are checked only for a tab's one pane and a zoomed client's
  focused pane: a pane the layout cannot fit is hidden and keeps its size,
  and `ls` does not say which panes are placed.
- **Overlays** are not checked across a resize: one drawn nowhere on a tiny
  screen appears when it grows.
- **Error notices** cut by a tiny bar to under four characters are not
  judged; a longer cut one must match a template's beginning.
- **The 9000×9000 client** waits up to 60 seconds for its listing:
  composing a 4096×4096 screen (16.7 million cells) takes seconds in a
  debug build.

## Traces

Each walk is saved in `--output` as `SECONDS-seed-N/`:

- `trace.txt`: `fux-walk trace 1`, comments, then one step per line in
  fux's word grammar, for example `keys c1 '\x02tn'`, `cli split -h -t %3`,
  `resize c1 2 2`, `child %2 burst 40`, `signal %2 stop`, `config
  prefix-a`. Bytes are printable ASCII as is, `\\` for a backslash and
  `\xNN` for the rest. Replay reads the steps, never the seed.
- `metadata.txt`: the seed, the fux binary's path, size and FNV-1a hash,
  `fux --version`, `uname -a`, the settle time.
- For a failure: `failure.txt` (the step, the invariant, what was seen, the
  end of the server's log; the fixture directory is kept too) and
  `minimized-NNN.txt`.

**Minimizing:** the shortest failing prefix by bisection, then steps
dropped in shrinking chunks while the walk still fails the same invariant,
each candidate on a fresh server, within `--minimize-seconds`.

No traces of fixed findings are kept in the repository.

## Tests

`cargo test --manifest-path walk/Cargo.toml` checks that the notice
templates come from fux's source and match what fux says.
