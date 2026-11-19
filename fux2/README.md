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
| `kill` | `fux2 kill build`, or a group: `fux2 kill --label project=x` | nothing, when everything it started is gone (see below) |
| `page` | `fux2 page build` | shows the pane's scrollback in a pager (`--close-pane`: then removes the pane it runs in; the viewer's `C-b [` uses this) |
| `view` | `fux2 view --layout tabs.toml` | the viewer |

With no program after `--`, `start` runs `$SHELL` (`%COMSPEC%` on Windows). The
program gets the caller's directory and environment, plus `TERM=xterm-256color`,
`TERM_PROGRAM=fux2` and `FUX2_PANE=<name>`. Panes are 80x24 unless told; a size
outside 1–256 columns and 1–128 rows is a usage error (exit 2). `--history BYTES`
is the memory history may use, 1 MiB unless told, at most 16 MiB (over it: exit
6). It is memory, not text: 1 MiB kept about 95 KB of short lines in a test.
A pane that has ended still counts toward the session's 128 until `fux2 kill`.
The session directory's path must leave room for a socket name: about 100 bytes
in all on Linux and macOS, said with the numbers if it is too long.

**`send`** takes any of these, in order, as one send that is queued whole or
refused whole (`busy`, at most 64 KiB waiting):

- `TEXT`, where `\n` is the Enter key as the program's terminal mode sends it,
  `\r` a carriage return, `\\` a backslash and `\xHH` a byte
- `--key NAME`: a key: `Enter`, `Tab`, `Escape`, `Space`, `BSpace`, `Up`,
  `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`,
  `Delete`, `BTab`, `F1` to `F35`, or a character; with `C-`, `M-` or `S-` in
  front for Ctrl, Alt and Shift (`C-c`, `M-x`)
- `--paste TEXT`: a paste, bracketed if the program asked for that; its text is
  taken as it is (`\n` there is a backslash and an `n`)
- `--stdin`: standard input, byte for byte

**`signal`** names are `HUP INT TERM KILL STOP TSTP CONT WINCH QUIT USR1 USR2`
and the other POSIX names (`ABRT`, `ALRM`, ...). On Windows only `INT` and
`QUIT` are delivered (as Ctrl-C and Ctrl-Break); the rest are `unsupported`.

**`kill`** hangs the program up, then kills its session (Windows: its job) and
waits for all of it. On Linux it also kills anything the program started that
left its session (`setsid`), since every such process stays the pane's. On macOS
a process that left the session can't be found, and is not killed. `kill` exits
1 and names what it could not end; exit 0 means nothing it found is left.
`kill --label K=V` kills every pane that matches, and exits 0 when none does.

**`read`** alone is a snapshot, so right after a `send` it may not show the
result yet. To wait in one command, give it the token `send` printed. Only rows
that changed after that token count for `--until` and `--until-line`, so a line
an earlier command left on the screen is never taken for this one's.
`--wait`, `--settle`, `--until`, `--until-line` and `--text` need `--since`, and
`--until` and `--until-line` need `--wait`:

| Flag | Meaning |
|---|---|
| `--since TOKEN` | answer with what changed since then, as JSON |
| `--wait MS` | wait for a change first; exit 5 if none comes |
| `--settle MS` | after a change, wait for this much quiet (default 100) |
| `--until TEXT` | keep waiting until a screen row contains TEXT |
| `--until-line TEXT` | keep waiting until a whole row equals TEXT (the safe choice: a typed command echoes on the screen, so `--until hi` matches `echo hi` itself) |
| `--text` | print the whole screen as text instead of JSON |
| `--json` | without `--since`: the whole screen and its metadata as JSON (`token`, `cols`, `rows`, `cursor`, `alternate`, `history` with `start`, `end` and `epoch`, `state`, `limitations`, and `screen`, a list of rows each with `row`, `text`, `rendition` and `spans`) |
| `--history-text` | all retained history and then the screen, as plain text |
| `--history EPOCH FIRST COUNT` | numbered rows as JSON, for programs; `EPOCH` and the retained range come from `--json` |

**`wait`** needs `--after TOKEN`; `--ms` is 30000 unless told, which is also the
most. At most 12 waits are parked on one pane at a time; a 13th is refused
`busy` (exit 6) at once.

A program that has ended keeps its last screen, readable until `fux2 kill`.

**Exit codes:** 0 success, 2 usage, 3 missing or replaced or gone, 4 stale,
5 timed out, 6 busy or over a limit, 7 unknown outcome (the command may or may
not have taken effect; it is never retried for you), 1 anything else, which
includes `exists` (that name is taken) and `ended` (the program has ended, so
it takes no keys). Errors go
to stderr as `fux2: <code>: <what>`.

## The viewer

`fux2 view` shows every pane, split, and types into the focused one. It sees new
panes within a second. A tile smaller than its pane clips it (the title says
so); a larger one leaves blank room. Viewing never resizes a pane.

| Keys | Action |
|---|---|
| `C-b o`, `C-b O` | next, previous pane |
| `C-b n`, `C-b p` | next, previous tab |
| `C-b 1` to `C-b 9` | that tab |
| `C-b l` | the tab shown before |
| `C-b )`, `C-b (` | next, previous workspace |
| `C-b L` | the workspace shown before |
| `C-b c` | a new shell in the shown tab |
| `C-b [` | the focused pane's scrollback, in a pager |
| `C-b f` | resize the focused pane to fit its tile |
| `C-b d` | detach |
| `C-b C-b` | type a literal `C-b` |

The focused pane's title bar is drawn in reverse video; the others are dim. The
status line says which workspace and tab are shown and how many there are, such
as `compiler 1/2 | logs 2/3`. Each viewer keeps its own place. The mouse is not
forwarded.

## Scrollback and copying

The viewer has no scroll mode. A pane's history is text you read with the tools
you already have:

```sh
fux2 read build --history-text | less
fux2 read build --history-text | wl-copy     # or pbcopy, or clip on Windows
```

That prints everything the pane still retains, then its screen, each line once
and in order, however the program scrolls meanwhile. A line the terminal
wrapped comes out as the one line the program wrote. A resize or a terminal
reset during the read starts it again; after three tries it says so (`stale`).
Above 256 MiB of text it refuses (`limit`).

In the viewer, `C-b [` opens the focused pane's history in a new pane (in the
shown tab, so not in a tab that matches `name=`; the stored layout of the next
release lifts that) running
`$PAGER`, else `less`, else `more` (`more.com` on Windows). Search and scroll
there, and select with your terminal to copy. The new pane is 80x24 like any other;
`C-b f` fits it to its tile. When the pager ends, the pane removes itself and the
focus returns.

### Layout file

`fux2 view --layout FILE` reads a small file in a subset of TOML, and rereads it
every second. A bad edit keeps the last good layout and shows the error, until
the file is good again. A tab is a filter on labels, and a workspace is a named
list of tabs. A pane that no tab matches is not shown. The subset: the sections
`[[tab]]`, `[[workspace]]` and `[[workspace.tab]]`, the keys `name`, `title`,
`match` and `axis`, double-quoted strings, one-line lists and `#` comments. A key
set twice in one section is refused.

```toml
[[tab]]
title = "compiler"
match = ["project=x"]     # labels that must all match; `name=build` works too
axis = "columns"          # auto, rows or columns
```

With several workspaces:

```toml
[[workspace]]
name = "compiler"

[[workspace.tab]]
title = "build"
match = ["project=x", "role=build"]

[[workspace.tab]]
title = "logs"
match = ["project=x", "role=logs"]

[[workspace]]
name = "docs"

[[workspace.tab]]
title = "all"
match = ["project=docs"]
```

`C-b c` starts a shell carrying the labels the shown tab matches, so it appears
there. In a tab that matches `name=`, no label can do that, and it says so. To end
a whole group: `fux2 kill --label project=x`.

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
| `(next-pane)`, `(previous-pane)`, `(next-tab)`, `(previous-tab)`, `(last-tab)`, `(go-to-tab N)` | move around |
| `(next-workspace)`, `(previous-workspace)`, `(last-workspace)` | change workspace |
| `(fit)`, `(detach)` | the viewer's actions |
| `(bind-verb CHORD WORD [N])` | bind a chord to one of those actions by name (or `"literal"`, which types the chord's first key). It acts at once, even while a binding runs |
| `(start-here PREFIX [PROGRAM ARG ...])` | a new pane named `PREFIX-N` with the shown tab's labels; gives `NAME@ID` |
| `(focus PANE)` | focus that pane (`NAME` or `NAME@ID`) when it appears within 10 s, and come back when it goes |
| `(fux2-program)` | the path of the `fux2` command, as a string |
| `(send PANE TEXT)` | types TEXT literally: write `\r` for Enter |
| `(send-key PANE KEY)`, `(signal PANE NAME)`, `(resize PANE COLS ROWS)`, `(kill PANE)` | as the commands |
| `(read PANE)` | the screen as one string |
| `(ls)` | the pane names, a list of strings |
| `(start NAME PROGRAM ARG ...)` | as the command; gives `NAME TOKEN` as a string |
| `(wait PANE TOKEN MS)` | as the command; gives `TOKEN STATE` as a string |

The actions give nothing back (`#inert`). A word that fails raises an error
that names what went wrong, shown on the status line. A binding copies at most
16 MiB of text out of the language in all, and a command's output is at most
8 MiB.

The default keys are this same language, in `fux2-view/src/defaults.grift` in
the source (built into the viewer), and any of them can be unbound or replaced.
`C-b [` is three lines there:

```lisp
(bind-key "C-b ["
  (lambda! ()
    (focus (start-here "history" (fux2-program) "page" (focused) "--close-pane"))))
```

Loading gets 2 seconds and each binding 5, on a thread of their own: the viewer
keeps drawing and taking keys, and `C-b d` works, while one runs. One that
overruns is stopped and the status line says so. An elevated viewer on Windows
does not run this file, since a normal process could have written it; the default
keys still work.

## Where panes live, and who may connect

The session directory is `$XDG_RUNTIME_DIR/fux2/default` on Linux (else
`~/.cache/fux2/default`), `~/Library/Caches/fux2/default` on macOS and
`%LOCALAPPDATA%\fux2\run\default` on Windows (`fux2-elevated` instead of
`fux2` for an elevated process), or the absolute private directory
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
clipboard bridge, no scroll mode in the viewer, no program-set palette colours, and
no judgement of whether a program is "ready".

Planned next, replacing the layout file described above: a layout stored in the
session directory that viewers and the command line can change, splitting by hand,
uneven splits with movable borders, moving panes between tabs, pickers for tabs and
workspaces, and a prefix menu built from the bindings. Today tiles are equal and a
pane's labels are fixed when it starts. If a pane's daemon itself is killed its screen is gone, and
its program normally gets a hang-up; a program that ignores that keeps running
with nothing attached.

On Windows, signals other than `INT` and `QUIT` are `unsupported`. If `fux2 start`
runs inside a job that forbids leaving it (some CI runners and sandboxes), it says
so on stderr before the program starts, and that pane ends with the job.
