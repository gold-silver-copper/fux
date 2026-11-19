# fux2

A terminal multiplexer with no server. Each pane is one small process that
owns a terminal, keeps its screen and answers whoever connects. A directory of
those panes is the whole session. The viewer is an ordinary client: start it,
kill it, start two, and the panes do not notice.

## Install and try

```sh
cargo install --path fux2 --locked
cargo install --path fux2-view --locked   # what `fux2 view` runs; keep the two side by side

fux2 start --name build -- sh
t=$(fux2 send build 'echo hi\n')
fux2 read build --since "$t" --wait 5000 --until-line hi --text
fux2 view        # C-b d leaves; the panes keep running
fux2 kill build
```

From a checkout, without installing: `cargo build -p fux2 -p fux2-view` and put
`target/debug` on your `PATH`.

## Commands

A *pane* argument is `NAME`, or `NAME@ID` for that one life of the name: if the
name has since passed to a new pane, the command is refused. Names match
`[a-z0-9][a-z0-9_-]{0,31}`.

A *token* is `<pane id>@<version>`, a place in a pane's life. `start` and `send`
print one. Mutating commands take `--if TOKEN` and are refused with `stale` if
the pane has moved on.

| Command | Example | Prints |
|---|---|---|
| `start` | `fux2 start --name build --label project=x --cols 100 --rows 30 --history 2000000 -- make` | `build <token>` |
| `ls` | `fux2 ls --label project=x --long` | a name per line; `--long` adds id, pid, size, state, labels |
| `send` | `fux2 send build 'make test\n'` | `<token>` |
| `read` | `fux2 read build` | the screen as text |
| `wait` | `fux2 wait build --after "$t" --ms 5000` | `<token> <state>` |
| `resize` | `fux2 resize build 100 30` | `100 30` |
| `signal` | `fux2 signal build INT` | `foreground-group`, or `console-input` on Windows |
| `kill` | `fux2 kill build` | nothing, when everything it started is gone |
| `view` | `fux2 view --layout tabs.toml` | the viewer |

With no program after `--`, `start` runs `$SHELL` (`%COMSPEC%` on Windows). The
program gets the caller's directory and environment, plus `TERM=xterm-256color`,
`TERM_PROGRAM=fux2` and `FUX2_PANE=<name>`. Panes are 80x24 unless told, at most
256x128. History is 1 MiB unless told, at most 16 MiB.

**`send`** takes any of these, in order, as one send that is queued whole or
refused whole (`busy`, at most 64 KiB waiting):

- `TEXT`, where `\n` is the Enter key as the program's terminal mode sends it,
  `\r` a carriage return, `\\` a backslash and `\xHH` a byte
- `--key NAME`: a key, such as `Enter`, `C-c`, `Up`, `F5`
- `--paste TEXT`: a paste, bracketed if the program asked for that
- `--stdin`: standard input, byte for byte

**`read`** alone is a snapshot, so right after a `send` it may not show the
result yet. To wait in one command, give it the token `send` printed:

| Flag | Meaning |
|---|---|
| `--since TOKEN` | answer with what changed since then, as JSON |
| `--wait MS` | wait for a change first; exit 5 if none comes |
| `--settle MS` | after a change, wait for this much quiet (default 100) |
| `--until TEXT` | keep waiting until a screen row contains TEXT |
| `--until-line TEXT` | keep waiting until a whole row equals TEXT (the safe choice: a typed command echoes on the screen, so `--until hi` matches `echo hi` itself) |
| `--text` | print the whole screen as text instead of JSON |
| `--json` | without `--since`: the whole screen and its metadata as JSON |
| `--history EPOCH FIRST COUNT` | up to 16 rows of history as JSON; `EPOCH` and the retained range come from `--json` |

A program that has ended keeps its last screen, readable until `fux2 kill`.

**Exit codes:** 0 success, 2 usage, 3 missing or replaced or gone, 4 stale,
5 timed out, 6 busy or over a limit, 7 unknown outcome (the command may or may
not have taken effect; it is never retried for you), 1 anything else. Errors go
to stderr as `fux2: <code>: <what>`.

## The viewer

`fux2 view` shows every pane, split, and types into the focused one. It sees new
panes within a second. A tile smaller than its pane clips it (the title says
so); a larger one leaves blank room. Viewing never resizes a pane.

| Keys | Action |
|---|---|
| `C-b o` | next pane |
| `C-b n` | next tab |
| `C-b f` | resize the focused pane to fit its tile |
| `C-b d` | detach |
| `C-b C-b` | type a literal `C-b` |

The mouse is not forwarded.

### Layout file

`fux2 view --layout FILE` reads tabs from a small TOML file, and rereads it every
second. A bad edit keeps the last good layout and shows the error.

```toml
[[tab]]
title = "compiler"
match = ["project=x"]     # labels that must all match; `name=build` works too
axis = "columns"          # auto, rows or columns
```

### Keys and commands: `view.grift`

The viewer loads `fux2/view.grift` from your config directory
(`$XDG_CONFIG_HOME` or `~/.config`, `%APPDATA%` on Windows), at most 64 KiB, in
the [grift](https://crates.io/crates/grift) language:

```lisp
(unbind-key "C-b o")
(bind-key "F6" (lambda! () (next-pane)))

(define! run-tests
  (lambda! () (send (focused) "cargo test\r")))
(bind-key "F7" run-tests)
```

| Word | Does |
|---|---|
| `(bind-key CHORD f)`, `(unbind-key CHORD)` | a chord is `"F7"` or `"C-b x"`; at most 256 |
| `(focused)` | the pane focused when the key was pressed, as `NAME@ID` |
| `(next-pane)`, `(next-tab)`, `(fit)`, `(detach)` | the viewer's actions |
| `(send PANE TEXT)` | types TEXT literally: write `\r` for Enter |
| `(send-key PANE KEY)`, `(read PANE)`, `(ls)` | as the commands |
| `(start NAME PROGRAM ARG ...)`, `(resize PANE COLS ROWS)` | as the commands |
| `(signal PANE NAME)`, `(wait PANE TOKEN MS)`, `(kill PANE)` | as the commands |

Loading gets 2 seconds and each binding 5, on a thread of their own: the viewer
keeps drawing and taking keys, and `C-b d` works, while one runs. One that
overruns is stopped and the status line says so. An elevated viewer on Windows
does not run this file, since a normal process could have written it.

## Where panes live, and who may connect

The session directory is `$XDG_RUNTIME_DIR/fux2/default` on Linux (else
`~/.cache/fux2/default`), `~/Library/Caches/fux2/default` on macOS and
`%LOCALAPPDATA%\fux2\run\default` on Windows, or the absolute private directory
`FUX2_DIR` names. A session holds at most 128 panes. It is refused unless it is a
real directory, yours and closed to others.

**Any process running as your user can connect, and a process that connects can
read every pane's screen, type into it and kill it.** On Windows the same holds
for processes of your user at the same elevation; an elevated and a normal
process have separate sessions. Other users are refused. Tokens and pane ids
prevent mistakes such as typing into a replaced pane. They are not passwords.

## What it leaves out

No shared layout or session state, no ordering across panes, no restart of a
program, no taking over of a pane from a crashed daemon, no mouse, no graphics, no
clipboard bridge, no program-set palette colours, and no judgement of whether a
program is "ready". If a pane's daemon itself is killed its screen is gone, and
its program normally gets a hang-up; a program that ignores that keeps running
with nothing attached.

On Windows, signals other than `INT` and `QUIT` are `unsupported`. If `fux2 start`
runs inside a job that forbids leaving it (some CI runners and sandboxes), it says
so on stderr before the program starts, and that pane ends with the job.
