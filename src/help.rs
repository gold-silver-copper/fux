//! `fux help`'s topics beyond the command list: one command at length, with
//! an example (`fux help wait`, `fux wait --help`), and the recipe for
//! driving programs from a script or an agent (`fux help agents`), which is
//! `docs/agents.md`, built in so that it is there with any binary and says
//! what this one does.

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");

/// The help for the command named `name`: its usage, what it does, and
/// more on its flags with an example where it has them; `None` if no
/// command has the name.
pub fn command(name: &str) -> Option<String> {
    let (usage, about) = crate::command::described(name)?;
    let mut out = format!("usage: fux {usage}\n");
    if !about.is_empty() {
        out.push_str(&format!("\n{about}\n"));
    }
    if let Some(more) = details(name) {
        out.push('\n');
        out.push_str(more);
        out.push('\n');
    }
    Some(out)
}

/// More on a command than its line in `fux help` says: its flags, and an
/// example.
fn details(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => {
            "\
Starts PROGRAM in place of a shell, in a tab of its own in workspace `agent`
(or -t WS: +N, or a name, made if missing), which no client is moved to.
Prints the new pane's id (--json: {\"workspace\",\"tab\",\"pane\"}).
  -n NAME           the tab's name (default: the program's)
  --size COLSxROWS  what the tab is laid out on while no client shows it
  --cwd DIR         where it starts (default: here)
  --env K=V         a variable for it; again for more
  --log FILE        everything it writes, kept in FILE past the history limit
  --wait-text TEXT, --wait-idle MS, --wait-exit, --wait-bell
                    then waits as `fux wait` does, from its first output on,
                    so nothing it prints at once is missed; prints the pane's
                    id, then what the wait says (--json: the wait's object)
When the program exits its pane stays, its last screen kept: `fux wait
--exit` prints `exited STATUS`, and send-keys to it fails.

  P=$(fux run --size 120x40 -- htop) && fux wait -t $P --text Load"
        }
        "wait" => {
            "\
Answers as soon as a pane (FUX_PANE without -t; -t again for any of several)
does any of what is asked:
  --text TEXT    shows TEXT, on the screen or in a line written since the wait
                 began that scrolled into history; prints that line
  --gone TEXT    no longer shows TEXT
  --idle MS      shows no change for MS (the same screen drawn again is none)
  --change       changes at all; with --since N, past revision N (--json)
  --exit         its program exits; prints `exited STATUS` or `killed by SIG…`
  --done         a shell command ends (OSC 133 marks); prints `done STATUS`
  --bell         it rings the bell or sends a desktop notification
  --timeout S    gives up after S seconds (default 60)
  --print        prints the screen as it ends
  --json         says what happened: {\"met\":\"text\",\"line\":…,\"status\":0}
A shell echoes typed commands: wait for text only their output has, or --done.
A program that exits first ends the wait (status 3), as a closed pane does (4);
a timeout is status 124. The server checks as output comes.

  fux wait -t %3 --text PASS --text FAIL --timeout 120"
        }
        "send-keys" => {
            "\
Each word is a key name (Enter, Escape, Tab, BSpace, Up, F5, C-c, M-x, BTab;
`fux list-keys` lists them) or, if it is not one, text; a word that looks like a
key name mistyped (Entr, C-Ent) is refused.
  --text TEXT        TEXT as typed, however it looks
  --paste TEXT       TEXT as a paste, bracketed if the program asked
  -l                 every word as text
  -N COUNT           everything COUNT times
  -c CLIENT          type at a client instead, as at its terminal
Then wait, armed before the keys go in, so nothing they bring is missed:
  --wait-text TEXT   until TEXT appears anew (not text already there)
  --wait-gone TEXT, --wait-change, --wait-exit, --wait-done, --wait-bell
  --wait-idle MS     until the screen shows no change for MS; status 5 if the
                     keys changed nothing
  --timeout S, --print, --json   as wait has them
A lone Escape and text are each read by the program on their own: Escape
Escape is two Escapes, and in `--text MSG Enter` the Enter submits.

  fux send-keys -t %3 --text 'make test' Enter --wait-text passed --wait-text failed"
        }
        "send-mouse" => {
            "\
A mouse event at a cell of the pane, rows and columns from 0, encoded as the
program asked; refused if it did not ask for mouse reports.
`click`, `right` and `middle` press and release; `press` and `release` the
left button alone; `wheel-up` and `wheel-down` one notch; `move` no button.

  fux send-mouse -t %3 click 4 10"
        }
        "capture-pane" => {
            "\
The pane's screen as text, trailing blanks and blank rows trimmed.
  -S -LINES     LINES of history first; -S all, all of it
  --full        every row and cell, untrimmed
  -J            rows the terminal wrapped joined into their lines
  --rows A-B    rows A to B only, from 0 (A alone: one)
  --cols A-B    columns A to B only, from 0
  --styles      colours and attributes, as SGR escapes
  --main        the main screen and its history, while a program holds the full
                screen: what was printed before it took it
  --alt         the full screen a program drew last, kept after it left it
                or exited (a menu's or editor's last screen)
  --last-output the last shell command's output alone (OSC 133 marks)
  --json        an object: size, cursor, lines (and with --styles, style
                runs per line), and what the program is doing: program, argv,
                cwd, exited, signal, idle_ms, revision, running, last_status,
                bells, notification, cursor_visible, alt_screen, mouse,
                bracketed_paste, copy_mode, history_lines, pictures

  fux capture-pane -t %3 --rows 0-5 -J"
        }
        "capture-client" => {
            "\
What a client's terminal shows: every row, the bar and overlays included.

  fux capture-client -c c1 --json"
        }
        "resize" => {
            "\
With -t %N, @N or +N: what the pane's tab, the tab, or every tab of the
workspace, is laid out on while no client shows it; `auto`: any client's screen. With -c CLIENT: a
headless client's size (`fux attach --headless`).

  fux resize -t +2 40x12"
        }
        "resize-pane" => {
            "\
-L, -R, -U or -D moves the nearest border that way, CELLS cells (default 1).
-x and -y make the pane a width or a height, in cells or as a percentage of
its tab; one the layout cannot give is refused, nothing changed.

  fux resize-pane -t %3 -x 95%"
        }
        "split" => {
            "\
Splits the pane: -h side by side, -v stacked. A command after -- is typed
into the new shell once it is ready. Prints the new pane's id.

  fux split -h -t %1 -- htop"
        }
        "new-tab" => {
            "\
A tab with a shell in workspace WS (default: here); a command after -- is
typed into it. Prints `@N %N`. --size COLSxROWS lays it out on that size
while no client shows it.

  fux new-tab -t +1 -n logs -- tail -f /var/log/syslog"
        }
        "new-workspace" => {
            "\
A workspace with a tab and a shell; a command after -- is typed into it.
Prints `+N @N %N`. Names are unique and do not start with +, %, @ or -.

  fux new-workspace -n work --size 100x30"
        }
        "kill-pane" | "kill-tab" | "kill-workspace" => {
            "\
Closes it, without asking; its programs are hung up.

  fux kill-workspace -t agent"
        }
        "ls" | "list" => {
            "\
Workspaces (+N), their tabs (@N) and panes (%N) with their sizes, and the
attached clients (cN). --json adds what each pane's program is doing."
        }
        "rename" => {
            "\
TARGET is %N, @N, +N or a workspace's name; quote a name with spaces.

  fux rename -t @2 'build logs'"
        }
        "move-pane" => {
            "\
--to sends the pane to a tab, a workspace's first tab, a new tab or a new
workspace; -L, -R, -U or -D beside its neighbour that way."
        }
        "swap-pane" => "Swaps two panes, anywhere: another pane by id, or the neighbour that way.",
        "reorder" => "Moves a pane, tab or workspace one place among its siblings.",
        "paste-buffer" | "show-buffer" | "list-buffers" => {
            "Paste buffers: copies, newest first (0), from copy mode and programs' OSC 52."
        }
        "set" => {
            "\
Options: prefix, shell, history-lines, clipboard, buffers, bell, titles; as
the config file has them.

  fux set history-lines 50000"
        }
        "bind" | "unbind" | "unbind-all" => {
            "\
Keys after the prefix (-n: alone), then the command they run, as the config
file has them.

  fux bind -n M-h select-pane -L"
        }
        "list-keys" => "Key names, for send-keys and set prefix, and the bindings.",
        "respawn" => {
            "\
Starts again, in its pane, the program `fux run` started there: the same words,
place and variables (--env adds or replaces; a program after -- replaces it).
The pane keeps its id, tab and size; its screen starts empty. The --wait-…
flags wait as run's do, from its first output on. Refused while
the program runs, unless -k, which ends it first.

  fux respawn -t %4 -k"
        }
        "signal" => {
            "\
Sends a signal to what runs in the pane's foreground, what its terminal's
keys reach: a job its shell runs, or the program `fux run` started; never a
shell itself. For a program that ignores Ctrl-C and hangups. TERM, KILL, INT, HUP, QUIT, USR1, USR2, STOP or
CONT, with or without SIG. The --wait-… flags wait as send-keys' do, armed
before the signal goes.

  fux signal -t %3 KILL --wait-exit"
        }
        "notify" => {
            "\
A notice on the screens of the clients showing TARGET (a pane, tab or
workspace), or every client's: for an agent working out of sight to say so.

  fux notify -t agent 'tests passed'"
        }
        "send-prefix" => "The prefix key, to the pane, as its program asked for keys.",
        "reload" => "Runs the config file again over the defaults; on an error, changes nothing.",
        "kill-server" => "Stops the server: every pane is hung up, and every client told why.",
        "detach" | "zoom" | "copy-mode" | "command-prompt" | "command-column" | "menu"
        | "rename-prompt" | "confirm-close" | "choose-tab" | "choose-workspace" | "choose-pane"
        | "select-pane" | "select-tab" | "select-workspace" => {
            "\
Acts on a client's screen: yours from a key or the prompt, -c CLIENT from the
command line (`fux ls` lists clients)."
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    /// Every `fux` command line in the README's and docs/agents.md's code
    /// blocks parses: the documentation names no command or flag fux does
    /// not take.
    #[test]
    fn documented_command_lines_parse() {
        for (doc, text) in [
            ("README.md", include_str!("../README.md")),
            ("docs/agents.md", super::AGENTS),
        ] {
            let lines = documented_lines(text);
            assert!(lines.len() >= 5, "{doc}: only {lines:?}");
            for line in lines {
                let words = crate::words::split(&line).unwrap_or_default();
                let parsed = crate::command::parse(&words);
                // A tab's name, which only a server can resolve, is fine.
                let named = matches!(
                    parsed,
                    Err(crate::command::Usage::Not(
                        crate::command::Kind::Pane | crate::command::Kind::Tab,
                        _
                    ))
                );
                assert!(parsed.is_ok() || named, "{doc}: `fux {line}`: {parsed:?}");
            }
        }
    }

    /// Every command has its help, and `fux help agents` is the recipe.
    #[test]
    fn every_command_has_its_help() {
        for (_, specs) in crate::command::COMMANDS {
            for name in specs.iter().flat_map(|spec| spec.names()) {
                let help = super::command(name).unwrap_or_default();
                assert!(help.starts_with("usage: fux "), "{name}: {help}");
                assert!(super::details(name).is_some(), "{name}: no details");
            }
        }
        assert!(super::AGENTS.contains("fux run"));
    }

    /// The command lines, `fux` taken off, in a document's code blocks: each
    /// `fux …` in a line, up to what ends it in a shell (`&&`, `;`, `|`, `)`, a
    /// comment), variables (`$P`) read as pane %1. `fux` alone, `attach`,
    /// `server`, `help` and `--version` are the binary's, not commands.
    fn documented_lines(text: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut code = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                code = !code;
                continue;
            }
            if !code {
                continue;
            }
            let line = line.split(" #").next().unwrap_or_default();
            for piece in line
                .split("&&")
                .flat_map(|p| p.split(';'))
                .flat_map(|p| p.split('|'))
            {
                let Some((_, after)) = piece.split_once("fux ") else {
                    continue;
                };
                let command = after.split(')').next().unwrap_or_default().trim();
                let first = command.split_whitespace().next().unwrap_or_default();
                if command.is_empty()
                    || ["attach", "server", "help", "--version", "--nested"].contains(&first)
                {
                    continue;
                }
                let words: Vec<String> = command
                    .split_whitespace()
                    .map(|w| {
                        if w.starts_with('$') {
                            "%1".to_owned()
                        } else {
                            w.to_owned()
                        }
                    })
                    .collect();
                lines.push(words.join(" "));
            }
        }
        lines
    }
}
