# fux

A terminal multiplexer with workspaces, tabs, split panes and stacked panes.
Rust owns terminals, layout and drawing. Grift supplies configuration, keys, the command
prompt, command lines and saved sessions.

Run `fux` to attach a terminal. The prefix is `C-b`; `C-b e` opens the command prompt,
and `C-b d` detaches. A pane keeps running after its client detaches.

| Keys | Action |
| --- | --- |
| `C-b v`, `C-b s` | Split side by side or stacked |
| `C-b h j k l` | Focus left, down, up or right |
| `C-b o`, `C-b q` | Next pane or last focused pane |
| `C-b z` | Zoom or restore |
| `C-b x` | Confirm closing a pane |
| `C-b t n`, `C-b n` | New tab or next tab |
| `C-b t r`, `C-b t g` | Rename or choose a tab |
| `C-b w n`, `C-b w g` | New workspace or workspace chooser |
| `C-b r h j k l` | Move a border |

A command line is one quoted grift program. Every following shell argument stays a string,
available through `(arguments)`. The command prompt accepts the same program syntax.

```sh
fux '(split (axis (pane 3) "horizontal"))'
fux '(line (pane 3) "ls -l")'
fux '(capture (pane 3))'
fux '(get (pane 3) "columns")'
fux '(line (pane 3) (first (arguments)))' 'echo "quotes (remain data)"'
```

Use a private server when driving programs beside someone else's terminal:

```sh
export TMPDIR="$HOME/.cache/claude-tmp/fux-example"
mkdir -p "$TMPDIR"
export FUX_SOCKET="$TMPDIR/fux.sock"
fux '(start)'
fux '(define! p (new-tab (program (size (in (options) "jobs") 80 24) "/bin/sh"))) (rename p "shell")'
fux '(line (pane "shell") "echo result=$((6*7))")'
fux '(shows (pane "shell") "result=42" 10)'
fux '(capture (pane "shell"))'
fux '(kill (workspace "jobs"))'
```

Actions return their pane's version from before the action. `changed` waits from that version;
`expect` refuses input after another change. `shows`, `quiet`, and `until-ended` operate on
observations. They report facts: text present, an unchanged version, or a process's exit.

`information` returns a held facts record; `snapshot` also offers topology and replay recipes.
Records print as grift data. Read their fields inside a program with `get`, or read printed
data with grift's `read-form`. A final list streams its items; `display` writes a whole value
as data. Diagnostics go to stderr and native errors use names such as `no-pane`.

A typing terminal loads `$XDG_CONFIG_HOME/fux/config.grift`, or
`$HOME/.config/fux/config.grift`. Its complete candidate is checked before becoming live;
a failure keeps the bundled keys.

A saved session is a readable grift program. Save with `fux '(dump (all))' > session.grift`;
recreate it on a newly started server with `fux --file session.grift`.

Help is `fux '(help)'`, `fux '(explain "split")'`, and `fux '(explain "agents")'`.
The [agent guide](docs/agents.md) covers observations, input, configuration and saving.
The [cookbook](docs/cookbook.md) gives complete examples.

This worktree builds against grift at `../grift/crates/grift`.
