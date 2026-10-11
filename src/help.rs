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
When the program exits its pane stays, its last screen kept: `fux wait
--exit` prints `exited STATUS`, and send-keys to it fails.

  P=$(fux run --size 120x40 -- htop) && fux wait -t $P --text Load"
        }
        "wait" => {
            "\
Answers as soon as the pane (FUX_PANE without -t) does what is asked:
  --text TEXT   shows TEXT (again for any of several)
  --gone TEXT   no longer shows TEXT
  --idle MS     shows no change for MS (the same screen drawn again is none)
  --change      changes at all
  --exit        its program exits; prints `exited STATUS`
  --timeout S   gives up after S seconds (default 60), exit status 1
  --print       prints the screen as it ends, timeout or not
The server checks as output comes: a wait costs nothing while it waits.

  fux wait -t %3 --text 'ready' --timeout 20 --print"
        }
        "send-keys" => {
            "\
Each word is a key name (Enter, Escape, Tab, BSpace, Up, F5, C-c, M-x, S-Tab;
`fux list-keys` lists them) or, if it is not one, text.
  --text TEXT        TEXT as typed, however it looks
  --paste TEXT       TEXT as a paste, bracketed if the program asked
  -l                 every word as text
  -N COUNT           everything COUNT times
  --wait-idle MS     then wait until the screen shows no change for MS
  --print            with --wait-idle: then print the screen
  -c CLIENT          type at a client instead, as at its terminal
A lone Escape and text are each read by the program on their own: Escape
Escape is two Escapes, and in `--text MSG Enter` the Enter submits.

  fux send-keys -t %3 --text 'git status' Enter --wait-idle 300 --print"
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
  -S -LINES     LINES of history first
  --full        every row and cell, untrimmed
  -J            rows the terminal wrapped joined into their lines
  --rows A-B    rows A to B only, from 0 (A alone: one)
  --cols A-B    columns A to B only, from 0
  --styles      colours and attributes, as SGR escapes
  --json        an object: size, cursor, lines (and with --styles, style
                runs per line), and what the program is doing: program,
                exited, idle_ms, alt_screen, mouse, bracketed_paste, pictures

  fux capture-pane -t %3 --rows 0-5 -J"
        }
        "capture-client" => {
            "\
What a client's terminal shows: every row, the bar and overlays included.

  fux capture-client -c c1 --json"
        }
        "resize" => {
            "\
With -t @N or +N: what the tab, or every tab of the workspace, is laid out
on while no client shows it; `auto`: any client's screen. With -c CLIENT: a
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
        "terminate" => "SIGTERM to what runs in the pane's foreground, not its shell.",
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
                assert!(parsed.is_ok(), "{doc}: `fux {line}`: {parsed:?}");
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
