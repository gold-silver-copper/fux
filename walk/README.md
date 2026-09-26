# fux-walk: seeded walks over a real fux

An excluded workspace with its own lockfile. It drives a real `fux` binary:
a fresh server, real `fux attach` clients on PTYs of their own, and the
command line. It takes seeded random steps, checks every invariant after
every step, saves each walk as a trace of the steps it took, and minimizes a
failing walk to the steps that matter. It is not built or run by fux's own
gates.

Its dependencies, besides fux, fux-vt and fuxix by path: `serde_json`, to
read `ls --json` (fux only writes JSON), and `signal-hook`, to stop cleanly
on SIGINT or SIGTERM between steps. It forbids the same lints as fux.

## Commands

From the repository root, with fux built:

```sh
cargo build --locked
cargo build --manifest-path walk/Cargo.toml

# Twenty walks of 300 steps, seeds 1 to 20:
walk/target/debug/fux-walk --fux target/debug/fux --seed 1 --seeds 20 --steps 300

# One long walk:
walk/target/debug/fux-walk --fux target/debug/fux --seed 777 --steps 3000

# Replay a trace, or every trace in a directory:
walk/target/debug/fux-walk --fux target/debug/fux --replay walk/traces/NNN-name.txt
walk/target/debug/fux-walk --fux target/debug/fux --replay-all walk/traces
```

- `--fux PATH` is required. It is made absolute before anything starts; the
  walk never builds fux and never looks it up on `PATH`.
- `--seconds N` bounds the whole run (default 3600), `--settle MS` how long a
  step may take to settle (default 5000; raise it under emulation), and
  `--minimize-seconds N` how long minimizing may take (default 900).
- `--output DIR` is where runs are saved (default `walk/runs/`, ignored by
  git). Exit status 1 means a walk failed, a replay failed, or the run was
  interrupted.

A debug build of fux is the one to walk: it checks arithmetic for
overflow.

## The fixture

Each walk and each replay starts a fresh `fux server` in a fresh 0700
directory under `/tmp` (a socket path is at most about 100 bytes, and
macOS's `$TMPDIR` alone is half of that). It has a config file the walk may
rewrite, `set shell /bin/sh`, an environment cleared but for `PATH`, with
`HOME` the directory, and `act.sh`, the script panes act through. The first
client is a real `fux attach`, 24 by 80, whose screen the walk parses with
fux-vt; up to two more attach and go during the walk.

Only what the walk started is ever signalled. Every wait is bounded, and
cleanup runs however a walk ends: the clients are killed, the server is
asked to stop and killed if it will not, and a pane process that outlives
the server is reported as a failure, never hidden.

## Steps

Each step is chosen with a small random index, resolved against the live
`ls --json`, and recorded as the concrete step it became:

- **keys a client types:** the prefix and a default binding, layers and
  repeat modes included; overlays driven with arrows, paging keys, Enter,
  Esc and their letters; copy mode's letters; command-prompt lines, some
  wrong; a lone Esc, the doubled prefix, an unbound letter, a capital, a
  letter with Ctrl; a bracketed paste; plain typing; `C-b d`;
- **commands** from the whole command set, with valid and invalid targets:
  new tabs and workspaces, splits, closing, renaming, moving, swapping,
  resizing, reordering, the `-c CLIENT` ones, `bind`/`unbind`/`set`,
  `reload`, `send-keys`, `capture-pane`, `capture-client`, `paste-buffer`,
  `terminate`, `detach` (never `kill-server`);
- **what no command expresses:**
  - a client's terminal resized to 1×1, 2×2, 1×200, 40×1 and ordinary
    sizes;
  - a client that asks for 9000×9000, speaking the protocol, which must get
    4096×4096;
  - a pane's program acting on its own through `act.sh`: entering and
    leaving the alternate screen, turning on mouse reporting, bracketed
    paste or application cursor keys, `stty` to another size, a burst of
    output, not reading its input for a while; or its shell exiting with a
    status;
  - SIGSTOP and SIGCONT to a pane's shell (each stopped pane is continued
    within a few steps, and all are before cleanup);
  - SIGHUP to a client, its terminal closed;
  - the config file rewritten (valid, invalid, another prefix), then
    `reload`;
  - clients attaching and going, one to three at a time.

While fewer than two panes are left, the walk splits one first, so that one
step can close at most what one step closes. A key path may still close the
last pane, and with it the server, as the README says it does: that walk
ends there, counted as ended, not failed.

## Settling and invariants

After each step the walk waits until it has settled, never with a bare
sleep: an act's marker, assembled when the script runs, shows in its pane;
and every client's painted screen equals its `capture-client`. Then:

- **the server** runs, answers, and its log has no panic;
- **structure,** from `ls --json`: ids are unique; every client's workspace,
  tab and focused pane exist and belong together; a client of a tab with
  panes focuses one;
- **processes:** a pane's shell ends within three seconds of its pane
  closing;
- **sizes:** a pane a client surely shows fits that client's pane area (see
  below);
- **the frame oracle:** every client's terminal shows, row for row, what the
  server composes for it (`capture-client`), within the settle time;
- **painting:** the screen has the client's rows, none wider than its
  columns, and no wide glyph starts in the last column; the bar names the
  client's workspace and tab where there is room, and says `[zoom]` when
  zoomed and nothing else claims its right side;
- **overlays:** a step that is not keys or a command opens none;
- **error notices:** every notice in fux's error colour matches a template
  from fux's source, every string literal in it with each `{…}` a wildcard
  (`src/notices.rs`). The list is never kept by hand.

Narrowed, and why:

- **Sizes** are checked only for a tab's one pane and a zoomed client's
  focused pane. A pane the layout cannot fit is hidden and keeps its size,
  as the README says, and `ls` does not say which panes are placed.
- **Overlays** are not checked across a resize: an overlay drawn nowhere on a
  tiny screen appears when it grows, so the screen before says nothing.
- **Error notices** cut by a tiny bar to fewer than four characters say
  nothing and are not judged; a longer cut one must match a template's
  beginning.
- **The 9000×9000 client** waits up to 60 seconds for its listing: composing
  a 4096×4096 screen, some 16.7 million cells, takes most of a second in a
  release build and seconds in a debug one, and the server peaked at about
  630 MB doing it.

## Traces

A run is saved in `--output`, as `SECONDS-seed-N/`:

- `trace.txt`: `fux-walk trace 1`, comments, then one step per line in fux's
  word grammar, for example `keys c1 '\x02tn'`, `cli split -h -t %3`,
  `resize c1 2 2`, `child %2 burst 40`, `signal %2 stop`, `config prefix-a`.
  Bytes are printable ASCII as they are, `\\` for a backslash and `\xNN`
  for the rest. Replay reads the steps, never the seed;
- `metadata.txt`: the seed, the fux binary's path, size and FNV-1a hash,
  `fux --version`, `uname -a`, the settle time;
- for a failure, `failure.txt` (the step, the invariant, what was seen, the
  end of the server's log; the fixture directory is kept too) and
  `minimized-NNN.txt`.

**Minimizing:** first the shortest failing prefix, by bisection, then
steps dropped in shrinking chunks while the walk still fails with the same
invariant, each candidate on a fresh server, within `--minimize-seconds`.

`traces/NNN-name.txt` are the minimized traces of fixed findings;
`--replay-all walk/traces` must pass.

## Tests

`cargo test --manifest-path walk/Cargo.toml` checks that the notice
templates come from fux's source and match what fux says.
