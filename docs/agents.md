# Driving programs with fux

(`fux help-agents`; `fux help` lists every word, `fux str WORD explain` one.)
A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs.

**Act, then read the answer.** Every action waits until the pane settles,
then answers with what happened: you never choose a wait or sleep to read.

```sh
fux opts str app name str './app --fast' command run   # start: answers %2 reading, then its output
fux %2 keys str y text str Enter key send-keys          # type: answers as run does
fux %2 str y type    fux %2 str Enter press             # the same, bundled
fux %3 str 'make test' line                             # a shell command: %3 reading · done 0, its output
fux %2 opts wait                                        # settle again (60s), what came since your input
fux %2 opts str 'listening on' wait-text wait           # wait for a text (case ignored)
fux %2 opts all-history capture                         # read: the screen once settled
fux %2 str state get                                    # one value: state, exited, pid, cwd, title
fux %2 opts json capture                                # also: main-screen, last-output, styles, marks
fux %2 str click 3 10 mouse send-mouse                  # a cell from 0, as a capture counts
fux %2 tab 80 24 resize    fux %2 50 percent resize-x   # sizes; also: opts horizontal … split
fux %2 str TERM signal     fux %2 opts respawn          # stop; start again on a new terminal
fux str agent workspace kill                            # at the end: everything run started, at once
```

The answer's first line is the pane and its **state**; the lines after it
are what the program wrote since you acted (your echo too), or the whole
screen of a full-screen program (an editor, a menu, a form):

```
%2 reading                     it waits for you: blocked reading its terminal, every key read
%2 reading · done 0            a shell command ended with status 0, and the shell reads again
%2 reading · no change         it read your keys and drew nothing: do not send them again
%2 waiting                     quiet 1s, but it may go on alone (network, a timer, a poll)
%2 busy · 12 bytes unread      still running or printing at the time limit (status 124)
%2 exited 3                    the program ended; its last output follows
```

- **Time:** an action or `capture` settles within 10s, a `wait` within 60s;
  `SECS timeout` changes it. `0 timeout` acts and answers at once, the
  state as it is (start many programs with it, then one `wait` on their tab
  or workspace). Busy at the limit: status 124; `PANE opts wait` waits on.
- **Start:** `run` starts a program (not a shell) in a tab of its own in
  workspace `agent`. OPTIONS: `str app name` (the tab's; one in use is
  refused), `100 30 size`, `str "$PWD" cwd` (a real path), `str K str V env`,
  and the program: `str 'prog -x a' command` (split as a shell splits), or
  `str PROG program` and `str A arg` per argument. The answer's first word is
  the pane: use it. Each `fux` is a fresh program (a `let` name lasts only
  within one). The pane stays after the program ends.
- **Keys:** names are tmux's (`Enter`, `Escape`, `Tab`, `Up`, `PageDown`,
  `BSpace`, `F5`, `C-c` Ctrl, `M-s` Alt, `S-Tab` Shift; a typo is refused).
  Inside `keys … send-keys`: `str T text`, `str NAME key`, `str T paste` (one
  bracketed paste), `str FILE paste-file`; `N times` repeats only the input
  just before it: `str Down key 2 times str Enter key` is Down Down Enter.
- **Waiting for a text:** `str T wait-text` (case ignored, blanks as one,
  from a word's start: `READY` is not in `NOT_READY`; `^T` a line's start,
  `T$` its end), `str T wait-gone`, `wait-exit`, `wait-bell`; several:
  whichever first. A wait counts what came since your last input to the pane
  (or since the program started), scrolled away too: it never starts too
  late. **Status 7:** the program reads its terminal and the text has not
  come: it cannot before you type; the answer is the screen. Status 3: the
  program exited first.
- **Read:** `capture` READ: `100 history`, `all-history`, `main-screen`
  (what was printed before a program took the screen), `alt-screen`,
  `last-output` (a shell command's output), `joined`, `marks`, `styles`,
  `json` (screen and state). Rows and columns count from 0.
- **Mouse:** `%2 str click ROW COL mouse send-mouse` (`right`, `wheel-up`,
  `press`, `release`, `move`), a cell counted from 0 as a capture counts:
  a program's own "row 22" hint is usually counted from 1.
- **Stop:** `%2 str TERM signal` (`INT`, `KILL`, …) answers once the program
  exits or settles; `%2 kill` closes one pane; `%2 opts respawn` starts it
  again, on a new terminal.

## Status

0 done (settled, or the wait met); 1 failed, `fux: CODE: MESSAGE` on stderr,
one line, with how the word is used; 3 the program exited before the wait;
4 the pane closed; 7 the program reads its terminal and the text has not
come; 124 still busy at the time limit. Words before a failing one have
already acted. `fux ls` (`ls json`, with titles and states) lists everything.

## A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program run let T
T str 'cargo test' line drop
T opts all-history last-output capture
```

`X let NAME` names a value, `define NAME … end` makes a word, `if … else …
then` and `begin … until` decide and repeat, `list … end` gathers what its
body leaves into a list, `SEQ each … end` runs its body once per element of
a string or list. An action leaves its answer: printed, it is the lines
above; given to a word that wants a pane, it is its pane; `string` makes it
text, so `%2 screen string str error contains` asks the screen a question.
The standard library, written in fuxth, does the rest: text (`words lines
split-on trim upper number join contains starts-with`), lists (`range sort
reverse sum count first last`), and `dup swap over rot not mod min max …`;
`fux help` lists every word, `fux help WORD` explains one. A mistake every
run would make (a word given too few values, a string given to `add` or
`if`) fails before any word acts.

## Shells

`fux opts str agent in new-tab` gives a shell (yours, else your login
shell). Each shell fux starts reads your own startup files, then marks its
commands (OSC 133): `line` answers `· done STATUS` once the command ends and
the shell reads again, `last-output` reads a command's output alone, and
`$?`, `PIPESTATUS`, your prompt and your hooks behave as without fux. A
command longer than the limit answers `busy` (124): `wait` on. A command
that prompts (`read`, `sudo`) answers `reading`, its prompt shown.
`str shell-marks str off set` turns marks off.

## Sizes

A tab no client shows is laid out on its `size` (an option of `run`,
`new-tab` and `new-workspace`); a program starts at its size. `TAB 40 12
resize` changes it later (a pane's: its tab; `resize-auto` goes back to a
client's screen). `PANE 95 percent resize-x` sizes a pane within its tab. A
size that cannot be had (over 500 a side, too small for a tab's panes, a
split with no room left) is refused, and nothing changes.

## Which server

`FUX_SOCKET` chooses the server; `str NAME server` (a server of your own,
`NAME.sock` in fux's directory) or `str PATH socket` first chooses another;
`start` starts one and returns once it answers; `status` says whether one
answers. Inside a person's pane, `here` is that pane, but only on that
pane's own server (`FUX_SERVER` says which): on yours it is not acted on. A
person can give you `str agent str PATH grant`: a socket that reaches
workspace `agent` alone, which they can watch in their own server (`str PATH
socket` first). `kill-server` from inside the server you would stop is
refused; `force-kill-server` stops it.

**In a sandbox** fux works as outside it. Where Unix sockets are refused
(Codex's sandbox), the client and server meet through a FIFO beside the
socket, `NAME.door`, in the same private directory; a server that cannot
bind its socket serves through the door alone. Where uids are remapped,
only the socket's own directory must be yours (mode 0700), and the client
checks that the server runs as you.

## More

- `opts str FILE log-to` on `run` keeps everything a program writes
  (`PANE str FILE start-log` from any time). `PANE str TEXT notify` shows a
  notice to people watching.
- `dump` prints what there is as a fuxth program that makes it again; run it
  with `fux FILE`. A crashed server's next run brings its panes back,
  programs waiting for `respawn`.
- `100 30 headless` is a client with no terminal (it prints its id, `c2`,
  and runs until stopped: start it in the background); `str c2 client keys
  … send-keys` types at it as a person would, its bindings included, and
  `str c2 client capture-client` shows what it shows: how to drive fux's own
  menus and prompts.
- Stop what you start by its pid (`kill $!`), not with `pkill -f`.
- fux takes no screenshots: a capture is text, styles and state. It never
  reads the clipboard.
