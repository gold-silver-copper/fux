# Driving programs with fux

fux runs programs in terminals and reports their screens, versions and exit states.
Every command line is one quoted grift program. Arguments after that program remain strings,
read with `(arguments)`. The config, keys, prompt and saved sessions use the same language.

Use a private server when driving programs beside another person:

```sh
export TMPDIR="${TMPDIR:-$HOME/.cache/claude-tmp}/fux-example-$$"
mkdir -p -m 700 "$TMPDIR"
export FUX_SOCKET="$TMPDIR/fux.sock"
fux '(start)'
```

Create a tab with an explicit workspace and size. The action returns its pane's version,
from before the action. A program pane remains after its process exits.

```sh
fux '(define! p (new-tab (program (size (in (options) "jobs") 80 24) "/bin/sh"))) (rename p "shell")'
fux '(line (pane "shell") "echo result=$((6*7))")'
fux '(shows (pane "shell") "result=42" 10)'
fux '(capture (pane "shell"))'
fux '(get (pane "shell") "columns")'
```

Close the workspace when finished:

```sh
fux '(kill (workspace "jobs"))'
```

A record prints as grift data. Read a field inside the program with `get`; a script can
read the printed data with grift's `read-form`, which evaluates nothing.

## recipes: Waiting for observations

`shows` first inspects the current capture, then waits for changes until the text appears.
Old output and input echo can match. Choose a result that the input itself does not contain.
`quiet` observes an interval without a version change. `until-ended` follows the program's
exit. Each timeout bounds one change; the OS's `timeout` can bound the whole command.

Save a version before deciding, and use `expect` to refuse input after another change:

```sh
fux '(define! p (new-tab (program (size (in (options) "guard") 80 24) "/bin/sh"))) (rename p "guard-shell")'
version=$(fux '(version (capture (pane "guard-shell")))')
fux '(line (expect (first (arguments))) "echo accepted")' "$version"
fux '(kill (workspace "guard"))'
```

## typing: Keys and bytes

`text`, `key` and `paste` build input on a target; `send-keys` submits the complete request.
`bytes` takes grift's immutable bytes value, including bytes that are not UTF-8.

```sh
fux '(define! p (new-tab (program (size (in (options) "typing") 80 24) "/bin/sh"))) (rename p "typing-shell")'
fux '(send-keys (key (text (pane "typing-shell") "echo typed") "Enter"))'
fux '(send-keys (bytes (pane "typing-shell") #bytes(3)))'
fux '(kill (workspace "typing"))'
```

## read: Retained facts

`information` is a facts snapshot. `snapshot` also accepts `"topology"` and `"recipe"` projections.
A record and its child records retain the facts of that observation. Reading a field of a
held record does not refresh it. Reading a field of a handle asks the server.

`capture` returns the visible text. Its options include `history`, `all-history`, `wrapped`,
`full`, `styles`, `marks`, `main-screen`, `alternate-screen`, `last-output`, `rows`, `columns`, and `since`.
A version contains respawns, screen changes, bells, completed shell commands, ended, and the run.
`changed` waits from that version. `expect` checks it before queuing input.

## programs: Configuration and callbacks

An attached typing client loads `$XDG_CONFIG_HOME/fux/config.grift`, or
`$HOME/.config/fux/config.grift`. The complete candidate config is checked before it becomes
live. A failure keeps the bundled keys and reports its file, line and value.

```grift
(set "history-bytes" 16777216)
(bind-key "C-b y" (code! (lambda! ()
  (split (axis (focused (me)) "horizontal")) #inert)))
(label "split right")
(group "Panes")
```

`include!` loads another file in the caller's context. `require!` runs a canonical file once.
Relative paths use the containing file. A client's later jobs use the sources frozen when
its config loaded. Callback forms use `code!`, with immutable captured arguments supplied
explicitly as `(list captured-arguments code-value)`.

At the command prompt (`C-b e`), type a grift program. The prefix followed by Escape stops
an active program, including a blocked wait. Rust keeps drawing while that worker runs.

## shells: Command marks

Shells can report prompt and command boundaries with OSC 133. fux reports those marks as
facts. `last-output` refuses with `no-marks` when the shell sent none. Readiness remains the
caller's decision.

```bash
# bash 4.4 or later (macOS's /bin/bash 3.2 has no PS0), ~/.bashrc
PS0='\e]133;C\a'
mark_end() { local s=$?; printf '\e]133;D;%s\a\e]133;A\a' "$s"; return "$s"; }
PROMPT_COMMAND=mark_end${PROMPT_COMMAND:+;$PROMPT_COMMAND}  # first: the hooks after it see $?
```

```zsh
# zsh, ~/.zshrc
autoload -Uz add-zsh-hook
mark_start() { print -n '\e]133;C\a' }
mark_end() { print -n "\e]133;D;$?\a\e]133;A\a" }
add-zsh-hook preexec mark_start
precmd_functions=(mark_end $precmd_functions)  # first, so $? is the command's
```

```fish
# fish, ~/.config/fish/config.fish
function mark_start --on-event fish_preexec; printf '\e]133;C\a'; end
function mark_end --on-event fish_postexec; printf '\e]133;D;%s\a' $status; end
function mark_prompt --on-event fish_prompt; printf '\e]133;A\a'; end
```

## servers: Saving and stopping

A saved session recreates programs, directories, names and layouts with new server identities:

```sh
fux '(dump (all))' > "$TMPDIR/session.grift"
```

Run it on a newly started server with `fux --file "$TMPDIR/session.grift"`.
`kill` closes a target; `kill-server` refuses an attached person or an invocation from one
of that server's panes. Each script should clean up the targets and processes it started.

A failure writes its kind, values and location to stderr and exits nonzero. Its report lists
native actions that already took effect. `timeout` exits 124; a closed waited-on pane exits 4;
a closed stdout pipe exits 141. Help is `(help)` or `(explain "split")`.
