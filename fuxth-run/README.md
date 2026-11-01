# fuxth-run

`fuxth`, a program that runs [fuxth](../fuxth) on its own: the language
with words for the terminal, files and the process, and no fux.

```sh
fuxth FILE ARGS…          # runs FILE: any file that reads, a FIFO or <(…) too
fuxth < FILE              # the same program, from stdin
cat FILE | fuxth          # and again
fuxth                     # stdin and stdout both terminals: the REPL
rlwrap fuxth              # the REPL with line editing and history
```

A program prints only what it prints (`emit`, `write`, `print`), as it
prints it. At its end the values left are printed as fux prints them, the
deepest first: a value a line, a list an item a line, a list in a list as
one line of JSON, nothing for an empty list or string. It exits 0. An uncaught failure says why on stderr, `fuxth:
CODE: PLACE: WORD: MESSAGE`, then each step that had already taken effect,
and exits 1; `N exit` exits N, what is left printed. Ctrl-C raises
`interrupt` (-28) in the program between slices of steps, after each word
the runner waited on, and during any wait for input (stdin, a FIFO), which a
`try` may catch; uncaught, it exits 130. Once stdout's reader is gone the
program ends quietly, with status 141, as `cat` does. fuxth sets no limits
of its own: `timeout`, `ulimit` and Ctrl-C do.

The REPL reads a line at a time, a quote left open going on to the next,
and after each prints the stack, then `ok`, or `compiled` while a
definition is open. An error says why and empties the stacks; the
dictionary stays. Ctrl-D ends it. It is fuxth's own loop, `begin
prompt-line evaluate false until`, so a definition goes on over lines.

`evaluate` is on.

## Words

| Word | Stack | |
|---|---|---|
| `read-key` | -- k | the next key: a character's code, or a key id below, with the masks of the modifiers held or'd in; Ctrl and a letter as the control character a terminal sends; a bracketed paste, of any length, a key a character. Fails `end_of_input` (-39) once stdin ends |
| `key-ready` | -- flag | whether `read-key` would not wait |
| `accept` | n -- s | the next line, at most n characters; the rest of a longer one is read next. A terminal edits and echoes it |
| `key-up` `key-down` `key-left` `key-right` `key-home` `key-end` `key-page-up` `key-page-down` `key-insert` `key-delete` `key-f1` … `key-f12` | -- k | the keys that type no character |
| `shift-mask` `ctrl-mask` `alt-mask` | -- n | the modifiers' masks |
| `cursor-at` | col row -- | moves the cursor, from 0 |
| `clear-screen` | -- | |
| `screen-size` | -- rows cols | the terminal's on stdin, else `LINES` and `COLUMNS`; fails `unsupported` (-21) without them |
| `read-file` | path -- s | a file's text, as in fux, a FIFO's too; fails `read` |
| `create-file` | path -- file ior | a new file, open to write; never one that exists |
| `write-file`, `write-line` | s file -- ior | s; and a newline |
| `close-file` | file -- ior | |
| `rename-file` | from to -- ior | |
| `file-mode` | path -- n ior | the permission bits |
| `set-file-mode` | n path -- ior | |
| `getenv` | name -- s | fails `unset_var` for one not set |
| `args` | -- list | the strings after FILE |
| `exit` | n -- | ends the program, what is left printed, with status n |
| `prompt-line` | -- s | the REPL's only: the stack and `ok`, then the next line |

An **ior** is 0, or a failure holding the OS's message (code `io`): `throw`
raises it, so `str x create-file throw` reads as in Forth. A word that
changed a file is listed with a failure after it.

## On a terminal

A key word puts the terminal in the mode keys need: no line editing or
echo, `IEXTEN` and `IXON` off, signals kept, set once with `stty`. It lasts
till the program ends, or, in the REPL, while the key word waits. From
then on Ctrl-Z stops the runner with the terminal's own modes back, set
again when it goes on, and SIGTERM, SIGHUP and SIGQUIT end it with them
back, its status 128 and the signal's number; every other way out puts
them back too. A lone Escape is told
from a key's escape sequence by fux's own delay, 35 ms, through fux-vt's
decoder. Off a terminal, stdin is read as UTF-8 and a sequence waits for
the rest however long it takes, so a file of keys drives a program:
`fuxth editor.fux notes.txt < keys`. Keys and lines (`accept`, the REPL)
take from one queue of what was read, in order: a key is decoded from as
few bytes as make it.

## Tested

`tests/runner.rs`: a batch program over a pipe prints only its output and
fails nonzero; a key-driven program over a pipe saves a file; Ctrl-C is a
failure `try` catches and exits 130 uncaught, and reaches a wait on a FIFO;
an ending signal puts the terminal back; the REPL on a terminal.
