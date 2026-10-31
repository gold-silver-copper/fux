# Cassettes

A cassette is a recorded session with fux, as an agent drives it: shell steps
that call `fux`, and the facts they must print. `tests/cassettes.rs` replays
every `NAME.cassette` here, each on a fresh private server from the build under
test, so `cargo test` (and the local gate) replays them all. Nothing in fux
changes for it: a cassette tests fux from outside, through its own interface.

```sh
cargo test --test cassettes                         # replay all
cargo test --test cassettes -- NAME…                # those whose names hold NAME
cargo test --test cassettes -- --update NAME        # record: fill in the `~` lines
cargo test --test cassettes -- --keep NAME          # leave its server running, and say where
cargo test --test cassettes -- --dir PATH           # cassettes from elsewhere (a PR's notes)
FUX_BIN=PATH cargo test --test cassettes            # against another build (the base)
```

`FUX_BASE=PATH` gives the steps `$FUX_BASE`, a second build, for a cassette
about two builds.

## The format

```text
# fux cassette 1
# guards: keys sent with expect at a version the pane has moved past are
#   refused stale, and none of them reach the program
# exercises: stale-expect
$ V=$(fux agent-opts 80 24 size str sh program str -c arg str 'read x; read y; echo "second=$y"' arg run)
$ fux "${V%%:*}" str x text str Enter key send-keys
  ~ %a:0.0.0.0.0:R1
$ fux str "$V" expect str y text str Enter key send-keys
  [1] stale
@journal
  = event=exited pane=%a exited=0
```

| Line | Meaning |
|---|---|
| `# fux cassette 1` | the first line |
| `# guards:` | **required:** the one property it protects. One cassette per property |
| `# fix:`, `# finding:` | where it came from (a PR, a finding) |
| `# timeout: N` | its bound in seconds, 1 to 60 (default 10), all its steps together |
| `# exercises: PROMISE…` | promises (below) it is written to exercise: uncovered, it fails |
| `$ …`, `> …` | a step and its continuation lines. All steps run in one `sh`, so variables carry |
| `  = TEXT` | **a check:** a line of the step's normalized stdout, after the previous `=` line's. `*` is any run of characters, `\*` a star |
| `  [N] CODE` | the step's exit status, and its last `fux: CODE:` if it failed. **Always checked;** none means 0 |
| `  ~ TEXT` | what the recording printed. Never checked: a replay that prints otherwise reports **drift** |
| `  ~? TEXT` | a line that differed between the two recordings: noise, which can't be a check |
| `@journal` | the journal, read whole after the steps. `= k=v …` checks some event has those top-level fields (`*` allowed), in order **per pane**; checks naming no pane, among events with none |

A step sees `DIR` (its 0700 directory, also `HOME`), `FUX_SOCKET` (its
server), `REPO` (this repository), `PATH` with the recording `fux` first,
`SHELL=/bin/sh`, `LANG=C.UTF-8` and no other variable of the caller's. Give
every new tab a size (`agent-opts 80 24 size`): there is no terminal. A step
may start a program in the background; it is killed with the cassette. A
cassette that closes its server's last workspace stops the server, which fails
it: keep a workspace of its own (`examples.cassette`).

## What a replay checks

1. **The `=` lines and every step's `[N]`,** the facts the author chose: an
   exit status, an error code, a count, a field, a text a program printed.
2. **The server lives:** it never ends during the replay, its log has no
   panic, it stops when asked, and no pane program outlives it.
3. **Every fux call** (each goes through `fux-record`, below) ends 0, 1, 4 or
   124, a failure with a code of fux's whose status is that status.
4. **fux's observable promises,** checked on raw values before anything is
   normalized, on fresh observations: an `ls` the runner takes before the first
   step and after each, causally after everything the step did.

   | Promise | Checked | Exercised by |
   |---|---|---|
   | `versions-grow` | per server run and program generation, each fresh observation of a pane had the one before (fux's `had`); every version a step printed is of this run, and one its pane had reached by the next observation | a count that grew |
   | `ended-frozen` | an exited program's BELLS, DONE, ENDED, status and signal never change until it is respawned, and SCREEN grows only by a `resized` or history event; no `bell`, `notification`, `title`, `done` or second `exited` in the journal | an ended pane observed twice |
   | `stale-expect` | a call with `expect` at a version the pane had already moved past is refused `stale`; a `stale` refusal names a version the pane did move past | such a call |
   | `wait-across-change` | a `changed … wait` from a version the pane had already moved past answers (status 0), never `timeout` | such a call |
   | `journal` | every event once, in order, none `missed`; nothing named before its `created` or after its `closed`; one `exited` per program | any event |

   A promise a replay never exercised is reported **uncovered**, never as
   held; the summary says which cassettes exercised each. The calls the
   promises read are found by their words (`expect`, `changed`, `wait`, and a
   version) in a call's arguments and stdin, not in a `--file`.
5. **Bounded time:** each cassette within its timeout, all within 60 s.

## Normalization

Before anything is compared, each line of output becomes:

| What varies | Becomes |
|---|---|
| a server run `PID-HEX16` | `R1`, `R2`… in order of appearance |
| `%3`, `@2`, `+1`, `c1` | `%a`, `@a`, `+a`, `ca`… by first appearance within their server run |
| a cursor or layout serial `N:R1` | `#:R1` |
| `"pid":N`, `"foreground_group":N`, `"foreground_leader":N` | `<pid>` |
| the cassette's directory, this repository | `$DIR`, `$REPO` |

## The lint

A check is a fact, never a picture: at most 16 `=` lines a step and 48 a
cassette; none that is a screen row (box drawing, or more than 60 % spaces),
a pid or a cursor; none in a step that waits with `quiet` (wait for a text, a
count or an exit instead). Print a fact: `str 'text' contains`, a count,
`grep -c`.

## Bounds

Each refused out loud, never cut silently:

| Thing | Bound |
|---|---|
| a cassette | 64 KiB, 64 steps |
| all cassettes | 32, 2 MiB |
| a step's stdout or stderr, a snapshot, a batch | 1 MiB (files a step writes: 2 MiB, `ulimit -f`) |
| the call log | 2 MiB, 1,024 calls; each call's fields cut to 4,000 characters |
| the journal | 4,096 events; panes in a snapshot, 256 |
| `~` lines | 40 a step, 200 characters each; 256 journal events |
| time | 10 s a cassette by default, at most 60; 60 s all together |

## Recording one

1. Write `NAME.cassette`: the header and the `$` steps you would type anyway.
2. `cargo test --test cassettes -- --update NAME` replays it twice, on two
   fresh servers, and writes each step's output as `~` lines (`~?` where the
   two differ). It never rewrites a `=` or `[N]` line: if one no longer holds,
   it stops and names it. It refuses to save the value of any of your
   variables named like a secret (`TOKEN`, `KEY`, `SECRET`, `PASS`, `AUTH`) or
   a token's shape, and never redacts.
3. Turn the `~` lines that show the property into `=` lines, add `[N]` lines
   and `@journal` checks, and replay.
4. A fix's cassette fails on the base build at its check and passes on the
   PR's: `FUX_BIN=$BASE/target/debug/fux cargo test --test cassettes -- NAME`.

An intended change of behaviour changes the `=` line in the same PR, which
says why. A cassette whose property a Rust test now checks, or another
cassette, is deleted: nothing is skipped.

## Verifiers (D-59)

`fux-record` is the recording shim: put it first in `PATH` as `fux`, with
`FUX_REAL` naming the real binary and `FUX_REC` a log file, and every call is
logged as one JSON line (argv, stdin, status, stdout, stderr, times).
`python3 -I quotes_check.py LOG TRANSCRIPT` checks that every quoted line of a
transcript (`> …`) is in the log.

```sh
mkdir -p "$TMPDIR/shim" && ln -sf "$REPO/tests/cassettes/fux-record" "$TMPDIR/shim/fux"
PATH=$TMPDIR/shim:$PATH FUX_REAL=$BUILD/fux FUX_REC=$OUT/calls-head.ndjson sh scenario.sh
python3 -I tests/cassettes/quotes_check.py "$OUT/calls-head.ndjson" "$OUT/transcript.md"
```

A verifier's scenario script becomes a cassette only through the promotion
filter: it runs, passes five times on head, meets its evidence rule (a fix
fails on base at its assertion and passes on head; preserved behaviour passes
on both; a new feature shows its absence on base), and checks a property
nothing else checks. Otherwise it stays with the PR's review notes.
