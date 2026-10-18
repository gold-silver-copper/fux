# Driving programs with fux

(`fux help-agents`; `fux help` lists every word, `fux str WORD explain` one.)
A fux command line is a fuxth program: words run left to right, each taking
what the words before it left. `str WORD` pushes WORD as text. `%2` alone is
a pane (`@2` a tab, `+1` a workspace). What is left is printed. The server is
`FUX_SOCKET`'s; `fux start` starts one if none runs.

**What an action takes goes on the pane, before it:** keys (`str y text`),
a wait (`str T wait-text`, `5 timeout`) and what to read (`all-history`),
in any order, then the word that acts (`send-keys`, `wait`, `capture`).
Only `run`, `new-tab` and `new-workspace`, which have no pane yet, start
from `opts`. A line that leaves keys or options nothing acts on is refused
before anything runs; a wait left last waits.

**Act, then read the answer.** Every action waits until the pane settles,
then answers with what happened: you never choose a wait or sleep to read.

```sh
fux opts str web name str './app --fast' command run    # start: answers %2 reading, then its output
fux %2 str y text str Enter key send-keys               # type (or str T paste): answers as run does
fux %2 str y type; fux %2 str Enter press               # the same, bundled
fux %3 str 'make test' line                             # a shell command: %3 reading · done 0, its output
fux %2 wait                                             # settle again (30s), what came since your input
fux %2 str 'listening on' wait-text wait                # wait for a text (case ignored); also wait-gone, wait-bell
fux %2 all-history capture                              # read: the screen once settled
fux %2 str state get                                    # one value: state, exited, pid, cwd, title
fux %2 capture json                                     # also: main-screen, last-output, styles, marks
fux %2 str C-c key send-keys                            # a key: Enter, C-c, Up, F5 (`fux key-names`)
fux %2 str click 3 10 mouse send-mouse                  # a cell from 0, as a capture counts
fux %2 tab 80 24 resize; fux %2 50 percent resize-x     # sizes; also: %2 horizontal split
fux %2 str TERM signal; fux %2 respawn                  # stop; start again on a new terminal
fux str agent workspace kill                            # at the end: everything run started, at once
fux ls                                                  # what there is: workspaces, tabs, panes
```

The answer's first line is the pane and its **state**; then what came since
you acted, from the line typed on if it changed (its prompt and your echo too;
else, as after a lone Enter or keys not echoed, from the first line that changed),
or the whole screen of a full-screen program (an editor, a menu, a form; one that scrolls as a shell, `koh connect`'s or mosh's, from the line typed on); one that redraws in place on the main screen answers from the top row it changed (its text or colours):

```
%2 reading                     it waits for you: blocked reading its terminal, every key read
%2 reading · done 0            a shell command ended with status 0, and the shell reads again
%2 reading · no change         it read your keys and drew nothing: do not send them again
%2 waiting                     quiet 1s, but it may go on alone: `PANE wait` waits up to 5s for more, then answers so (status 0); `· redraws on its own` (a clock, `top`): it read your keys and drew after them, a later capture may differ, `str TEXT wait-text` waits for a text
%2 busy · 12 bytes unread      still running, printing or asleep on a timer (a sleep loop) at the time limit (status 124); unread: input fux holds yet
%2 exited 3                    the program ended; what it printed since your input follows
```

- **Time:** an action, a `capture` and a `wait` all wait 30s at most (the server's
  `wait-seconds`); `SECS timeout` on the pane changes it for one (`%2 0 timeout str ls line`). `0 timeout` acts and answers
  at once, the state as it is (start many programs so, then one `wait` on their tab or
  workspace waits for each pane, a line each, the first failing status). Busy at the limit: status 124; `PANE wait` waits on. A shell command of unknown length (a build quiet for minutes): `wait-done` waits as long as it runs, with no limit but a `timeout` given, to its end (`· done STATUS`) or to the text first: `%2 str 'build ok' wait-text wait-done wait` (status 5 if it ended first). Wait words go before the action they are for; a wait alone on the pane, last on the line, waits by itself (`%2 str ready wait-text` is `%2 str ready wait-text wait`).
- **Start:** `run` starts a program (not a shell) in a tab of its own in
  workspace `agent`. OPTIONS, after `opts`: `str web name` (the tab's name,
  here `web`; one given already
  is refused, one fux gave, `tab-2`, is taken), `100 30 size`, `str "$PWD" cwd` (a real path), `str K str V env`,
  and the program: `str 'prog -x a' command` (split as a shell splits), or
  `str PROG program` and `str A arg` per argument. The answer's first word is
  the pane: use it. Each `fux` is a fresh program (a `let` name lasts only
  within one). The pane stays after the program ends. A word refuses options it would not use (`capture` a wait): `unused`. `run` answers once the program prints, or reads quiet a second having printed nothing (ssh and `koh connect` read as they connect).
- **Keys:** names are tmux's (`Enter`, `Escape`, `Tab`, `Up`, `PageDown`,
  `BSpace`, `F5`, `C-c` Ctrl, `M-s` Alt, `S-Tab` Shift; no keypad keys; a typo
  is refused), always after `str`. Between the pane and `send-keys`: `str T text`, `str NAME key`, `str T
  paste` (bracketed if the program turned that on, an end-of-paste mark in T
  then dropped), `str FILE paste-file`; 900 KiB at most in all, repeats too
  (text its bytes, a key its name's, each input after the first 5 more). `N times` (1 to 1000) repeats only the input just before it: `str Down key 2 times str Enter key` is Down Down Enter.
- **Waiting for a text:** `str T wait-text`, `str T wait-gone`, `wait-exit`,
  `wait-bell`; several: whichever came first, as they came (fux records
  each piece of output as it arrives, and where a bell rang among them). A
  text: case ignored, blanks as one, from a word's start (`READY` is not in
  `NOT_READY`); `^T` a line's first word, indented or not, `T$` its last.
  Met, it answers the state line, ending with what met it (`%2 busy · met
  "READY"`), then the line that holds the text (`wait-gone`: what came; after
  `run` or `respawn` a text must show before it can go). Failed (exited first,
  timed out), it answers the state line and the pane's last lines, and says
  why on stderr. A wait counts what the program printed since your
  last input or signal (or since it started): lines scrolled away, and
  text drawn over in the same breath (`printf 'READY\rxxxxx'`), too; it
  never starts too late. It never matches what you typed: the echo of your
  keys (the terminal's own, for a program that reads lines; the shell's as
  it reads its command line; a line editor's, drawn where you typed) is no
  output, whatever it reads like. What the program prints in reply is,
  though it repeats your words (`You said: NEEDLE`). A line drawn again to
  read as it did before your input brings nothing new, within the first
  MiB of output after it; past that, in a flood, every line printed is. **Status 7:** the
  terminal's foreground
  program is blocked reading it, and the text has not come (`wait-gone`:
  not gone). This is fux's judgement from what the program is blocked in:
  one also woken by a timer (`alarm`) or another descriptor gets 7 too,
  though it may go on alone, as may a program left in the background (look
  again later, or use `wait-exit`); one with another thread at work is
  busy, and the wait runs on. One reading the terminal or a pipe while
  another thread sleeps is `waiting` (it may go on alone): a settling
  action answers status 0 after 1s, and a text wait runs to its limit
  (124), never 7. A read with the terminal's own time
  limit (VTIME, or the gap after its first key) is `waiting`, not 7: it
  returns alone, so a text wait runs to its limit (124). The answer is the
  screen (it starts `… earlier lines not kept (past history-bytes)` if lines
  written since were let go, or `… more came than the pane's journal keeps
  (journal-bytes): text may have passed unseen`, 32 MiB by default). Status 3: the program exited first.
  A met `wait-exit` answers status 0, `%2 exited 3` and what came: the
  program's own status is in its state (`str exited get`).
- **Read:** `PANE READ capture`, READ any of: `100 history`, `all-history`, `main-screen`
  (what was printed before a program took the screen), `alt-screen`,
  `last-output` (a shell command's output; while one runs, its output so far:
  a full-screen one's is its screen to the cursor's row, whole; one of
  `main-screen`, `alt-screen` and `last-output` at most), `joined` (with `rows`, a
  wrapped line touched is taken whole, the cursor with it), `marks`, `styles`,
  `json` (screen and state). Rows and columns count from 0. Keys after
  which a program leaves its full screen answer what it printed after (in
  whole rows: old text on the row it starts on too), else that full screen if they changed it, else the screen as it is.
- **Mouse:** `%2 str click ROW COL mouse send-mouse` (`right`, `wheel-up`,
  `press`, `release`, `move`), a cell counted from 0 as a capture counts:
  a program's own "row 22" hint is usually counted from 1.
- **Stop:** `%2 str TERM signal` (`INT`, `KILL`, …) answers once the program
  exits or settles; `%2 kill` closes one pane; `%2 respawn` starts it
  again, on a new terminal.

## Keys a program reads one by one

To a program that reads each key itself (raw, on the main screen: a line
editor, a REPL), or one whose echo fux cannot work out, each text and key
goes in once it took the one before, so that what it draws follows the key
it answers: it may read them in several reads, the same bytes in the same
order. A wait sent with them answers once all went in. A full-screen
program, and one reading lines, gets them at once. A shell fux starts reads its
command line key by key too: of several lines sent to it at once, each goes
in as it reads again, or after a second, so a line may go in while the one
before still runs, the terminal showing it among that command's output.

## What fux cannot tell

fux records which input each piece of output followed, and tells echo
from output by the shell's marks, the terminal's own echo rules, and what
the input was. A few cases look the same either way, and are taken so:

- A full-screen program's screen is all output: a form's field showing
  what you typed meets a wait for it (wait for it to know the keys went
  in).
- Until a key submits the line you type (Enter), a text made only of
  characters you typed on it (in any order: editing keys move them; case
  ignored, as a wait ignores it) is taken as their echo, as a line editor
  shows them, wherever it lands: a program that answers your keys with
  nothing but some of them is not seen. The text you wait for is what
  counts, not the line it is on: `RESULT TOKEN` printed before Enter meets
  a wait for `RESULT TOKEN`, not one for `TOKEN`.
- A program that, after Enter, prints exactly the line you typed: taken
  as its output, its reply to Enter. (A line editor drawing your line again
  on Enter, the same text in the same place, changes nothing: no output.)
- Keys typed ahead, with Enter, into a program fux does not mark (a shell
  over ssh, or a line editor still busy or asleep when they went in): its
  drawing them again once it reads them reads as output that repeats them.
  A shell fux starts marks its prompt, and says which.
- Keys typed just as a command ends: the terminal may echo them after
  the command's last output, and when that output holds the same
  characters in the same order, those are taken as the echo.

## Status

Each status means one thing, its code naming which. 0 done (settled, or
the wait met); 1 failed, `fux: CODE: MESSAGE` on stderr, one line (a line
end in it shown as `\n`), with how the word is used when one of fux's own
words was given too few values or the wrong kind; 2 a command used wrongly
(`usage`); 3 the program exited (or was started again) before the wait;
4 the pane closed; 5 the shell command a `wait-done` lasted for ended before
the text came; 7 the program is blocked reading its terminal and the
text has not come (or gone); 124 still busy at the time limit (with `0 timeout`, a wait for a text
or the exit not met at once). A met wait for the exit (`wait-exit`) is 0:
the program's own status is in its state line (`%2 exited 3`) and in
`str exited get`, never fux's (nor `fux FILE`'s). A run that does not
fail exits with the status of the first answer it leaves that has one (as
itself, its `json`, or in a list): an answer dropped (`wait drop`) or
taken by a word gives it none. Words before a failing one
have already acted, unless the mistake is one every run would make
(below, under A program): that is refused before anything runs. `fux ls` (`ls json`, with titles and states) lists everything.

## A program

A file of fuxth runs with `fux FILE` (or `fux -` from stdin), and keeps its
names between steps:

```sh
# test.fux
opts str tests name str bash program run let T
T str 'cargo test' line drop
T all-history last-output capture
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
run would make (a word given too few values, a string written just before
`add` or `if`, a pane's word given options, a capture given to `text`, a
request left that nothing acts on, or a word that always leaves the wrong
kind) fails before any word acts; a value whose kind cannot be known
before (from a list, or a word that leaves either) is checked as it runs.
A word may call one defined after it, and a definition may take a library
word's name.

## Shells

`fux opts str agent in new-tab` gives a shell (yours, else your login
shell). Each shell fux starts (bash, zsh or fish, interactive, given no
script, command or rc file of its own) reads your own startup files, then
marks its commands (OSC 133): `line` answers `· done STATUS` once the command ends and
the shell reads again, `last-output` reads a command's output alone, and
`$?`, `PIPESTATUS`, your prompt and your hooks behave as without fux. A
command longer than the limit answers `busy` (124): `wait` on. A command
that prompts (`read`, `sudo`) answers `reading`, its prompt shown.
Output with no line end of its own is followed by the prompt on a row of
its own. bash marks from `PROMPT_COMMAND` and `PS1`: a `PROMPT_COMMAND` set anew in
the running shell has `last-output` take what it prints too (a new `PS1`
changes nothing); with both
replaced, or made readonly in a startup file (before fux sets them), a
command never ends: `line` answers `reading` with no `done`, and
`command_running` stays true; made readonly later, they keep fux's marks.
`str shell-marks str off set` turns marks off for shells started after.

A shell's marks carry a nonce, so output (`cat file`) cannot pass for
them. A program you run in it on purpose that marks commands of its own
(a remote shell's, relayed by `koh connect` or ssh; a nested multiplexer)
is given another nonce in `FUX_MARK_NONCE`; its OSC 133 marks with
`;nonce=$FUX_MARK_NONCE` among their options (`ESC ] 133 ; D ; 1 ; nonce=N
BEL`) count while the command that started it runs: `line` typed into it
answers `· done STATUS`, `last-output` reads its command's output, and when
it exits the shell's own end ends that command. Its marks without the
nonce, or before it ran, count for nothing. A program `run` in a pane of
its own needs no nonce: its marks always count.

Only the action whose input began a marked command waits for it to end:
`line` (or keys ending in Enter at a prompt), and a bare `wait` after it.
Keys sent into a command that runs on (a pager, an editor, a REPL, a
prompt, local or remote), and a `capture`, settle as with any program:
once it reads, or is quiet a second. One that redraws by itself (a clock,
`top`: it drew alone twice in a row, frames at most 2s apart), polling its
terminal between frames or on the alternate screen, is never quiet: it
settles once it read every key and the frame it drew after them is done,
`%2 waiting · redraws on its own`, and the `line` that typed it once it
drew so. That frame is the first after your keys,
which is mostly their answer, but what it draws later may differ: wait
for the text you need with `str TEXT wait-text`. A pane that settles while a marked
command runs on says so: `%2 waiting · command running`. A command `line` typed
that waits for keys answers so too: one reading its terminal (`read`, a
pager), and one polling it with a time limit as it edits a line (bracketed
paste on, as Python's REPL has it), once quiet a second, every key read. One
polling it otherwise (a pause that watches for keys, drawing nothing on its own), or blocked elsewhere
(a download, a pipe), is waited for to its end: `· done STATUS`. A command `line` typed
that takes the full screen after it began (`less`, `vim`, an ssh session's
too) answers once it reads or is quiet, its screen shown. One that a
full-screen program marks itself (its `C` came on the alternate screen, as
a remote shell's relayed by `koh connect` do) is waited for to its end,
though the program waits on the network meanwhile: `line` answers
`· done STATUS`, or `busy` at the time limit. A relay that can see the
remote program says what it does, `ESC ] 133 ; S ; activity=STATE ;
nonce=N BEL` (STATE `reading`, `polling`, `waiting`, `busy`, `idle` or
`unknown`, as fux's own probe has them; the nonce as for its marks), as
`koh connect` does: its last report stands for fux's probe while no mark
came after it and it came after your last input, so a remote `less`,
`sudo` or REPL answers `reading` as a local one does, its `line` too.
Without one, fux cannot tell a remote command that waits for input there
(through ssh) from a slow one: its `line` answers `busy` at the limit.
Type such a command with `0 timeout`, or wait for its prompt with
`wait-text`; the keys you send it next settle at once.

## Sizes

A tab no client shows is laid out on its `size` (an option of `run`,
`new-tab` and `new-workspace`); a program starts at its size. `TAB 40 12
resize` changes it later (a pane's: its tab; `resize-auto` goes back to a
client's screen). `PANE 95 percent resize-x` sizes a pane within its tab. A
size that cannot be had (over 500 a side, too small for a tab's panes, a
split with no room left) is refused, and nothing changes. A client's size
is its terminal's, at most 4096 a side.

## Which server

`FUX_SOCKET` chooses the server; `str NAME server` (a server of your own,
`NAME.sock` in fux's directory) or `str PATH socket` first chooses another
(a socket's path is at most 107 bytes, 103 on macOS: a longer one is
refused, door and all); `start` starts one and returns once it answers; `status` says whether one
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
  with `fux FILE`. A shell pane comes back as a new shell in its directory:
  what was typed into it, its `typed` line too, is not typed again. It adds to what the server has: on a new server, its own
  `main` tab stays beside the dump's, which then takes the next name
  (`tab-2`; a name the dump gives takes one fux gave, which is named anew);
  kill the tab you do not want. A crashed server's next run brings its panes back,
  programs waiting for `respawn`.
- `100 30 headless` is a client with no terminal (it prints its id, `c2`,
  and runs until stopped: start it in the background); `str c2 client …
  send-keys` types at it as a person would, its bindings included, and
  `str c2 client capture-client` shows what it shows: how to drive fux's own
  menus and prompts.
- Stop what you start by its pid (`kill $!`), not with `pkill -f`.
- fux takes no screenshots: a capture is text, styles and state. It never
  reads the clipboard.
