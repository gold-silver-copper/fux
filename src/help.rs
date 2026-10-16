//! fux's help, made from its words (`crate::script::WORDS`) and the bundled
//! library's comments, so that it always says what this binary does; and
//! the recipe for driving programs from a script or an agent
//! (`help-agents`), which is `docs/agents.md`, built in.
use crate::script::{LIBRARY, WORDS};

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");
/// What `json` gives, word by word (`help-json`).
pub const JSON: &str = include_str!("../docs/json.md");

/// How fux runs, and every word under its heading.
pub fn overview() -> String {
    let mut out = String::from(
        "\
usage: fux                 attach, starting a server if none runs
       fux WORD...         run the words, a fuxth program
       fux FILE            run a file of fuxth (a word with a / or ending .fux)
       fux -               run fuxth read from stdin

fuxth is a small Forth: words run left to right, each taking what the words
before it left. A number is a value; `%2`, `@2`, `+1` alone are a pane, a tab,
a workspace; `str WORD` pushes WORD as text, however it looks; any other bare
word runs. What is left at the end is printed, a value a line. An action
answers once its pane settles (30s at most, as a wait waits: the server's
`wait-seconds`; `SECS timeout` changes it, `0 timeout` answers at once). A
failure prints `fux: CODE: MESSAGE` on stderr, with how the word is used
(one of fux's own); an action's or a wait's status is the exit status (3
exited first, 4 closed, 7 the program is blocked reading its terminal and
the text has not come, 124 still busy), any other failure 1. Each `fux` is a fresh program.

Most steps have one of these shapes (UPPER = what you fill in):

  fux opts OPTIONS run                  start a program; answers once it settles: %2 reading, then its output
  fux %2 keys INPUT send-keys           type: str T text, str Enter key, …; answers as run does
  fux %3 str 'make test' line           a shell command: answers `%3 reading · done 0` and its output
  fux %2 opts [WAIT] wait               settle, or wait for str T wait-text, wait-exit, …
  fux %2 opts READ capture              read once settled: 100 history, marks, json, …
  fux %2 str FIELD get                  one value: state, exited, pid, …

  fux opts str 'htop -d 5' command run  start htop
  fux %2 str Load await                 wait until it shows Load
  fux ls                                what there is

The language's core: str WORD   define NAME … end   let NAME (names the
  top value)   if … else … then   begin … until   list … end (a list of
  what its body leaves)   each … end (the body once per element)
  drop pick roll   add sub mul div   lt eq   len at slice concat push put
  code char   kind string print fail   # a comment to the end of the line
  Everything else (dup swap over rot, not, mod, sort, upper, join, …) is
  the standard library, written in fuxth: listed below.
  Mistakes a run would surely make (too few values, a string given to add
  or if) are found before anything runs.

At a terminal, the prefix then Esc stops a program a key or the prompt runs.
Driving programs from a script or an agent: `fux help-agents`.
One word at length: `fux str WORD explain`.
",
    );
    let mut group = "";
    for word in WORDS {
        if word.group != group {
            group = word.group;
            out.push_str(&format!("\n{group}:\n"));
        }
        out.push_str(&format!(
            "  {:<20} {:<28} {}\n",
            word.name, word.effect, word.about
        ));
    }
    out.push_str("\nThe standard library and fux's library (both written in fuxth):\n");
    for (name, effect, about, _) in library() {
        out.push_str(&format!("  {name:<20} {effect:<28} {about}\n"));
    }
    out.push_str(
        "\nThe server: `str NAME server` or `str PATH socket` before the rest;\n\
         else FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux. Inside a\n\
         pane, `here` is that pane (FUX_PANE).\n",
    );
    out
}

/// The prelude's and library's words: each name, stack effect, what it
/// does, and its definition, from the comment before its `define`:
/// `# pane text -- : what it does`. A definition over several lines is
/// its indented body and the `end` closing it.
pub fn library() -> Vec<(&'static str, &'static str, &'static str, String)> {
    let mut words: Vec<(_, _, _, String)> = Vec::new();
    let mut comment = None;
    let mut open = false;
    for line in fuxth::STDLIB.lines().chain(LIBRARY.lines()) {
        if open && (line.starts_with("  ") || line == "end") {
            if let Some((.., code)) = words.last_mut() {
                code.push_str("\n  ");
                code.push_str(line);
            }
            open = line != "end";
            continue;
        }
        open = false;
        if let Some(text) = line.strip_prefix("# ") {
            comment = text.split_once(" : ");
        } else if let Some(rest) = line.strip_prefix("define ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if let Some((effect, about)) = comment.take() {
                words.push((name, effect, about, line.to_owned()));
                open = true;
            }
        }
    }
    words
}

/// One word at length: its stack effect, what it does, and an example.
pub fn explain(name: &str) -> Option<String> {
    if let Some(word) = WORDS.iter().find(|w| w.name == name) {
        let mut out = format!("{}  ( {} )\n\n{}.\n", word.name, word.effect, word.about);
        if let Some(usage) = crate::script::usage(name) {
            out.push_str(&format!("\n{usage}\n"));
        }
        if let Some(takes) = options(name) {
            // What they go on: send-keys' input, send-mouse's event.
            let on = match name {
                "send-keys" => "keys (opts is refused)",
                "send-mouse" => "mouse",
                _ => "opts",
            };
            out.push_str(&format!("\nIts options, after {on} and before {name}:\n"));
            for option in takes {
                if let Some(w) = WORDS.iter().find(|w| w.name == option) {
                    out.push_str(&format!("  {:<14} {:<26} {}\n", w.name, w.effect, w.about));
                }
            }
        }
        if let Some(example) = example(name) {
            out.push_str(&format!("\n  {example}\n"));
        }
        return Some(out);
    }
    if let Some((name, effect, about, code)) = library().into_iter().find(|(n, ..)| *n == name) {
        return Some(format!(
            "{name}  ( {effect} )\n\n{about}.\nWritten in fuxth:\n\n  {code}\n"
        ));
    }
    if let Some(builtin) = fuxth::Builtin::ALL.iter().find(|b| b.name() == name) {
        return Some(format!(
            "{name}  ( {} )\n\n{}. One of the core's words.\n",
            builtin.effect(),
            builtin.about()
        ));
    }
    let (shape, about) = syntax(name)?;
    Some(format!("{name}  {shape}: {about}\n"))
}

/// A word of the language's own syntax: how it is written, and what it
/// does.
pub(crate) fn syntax(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "if" | "else" | "then" => ("BOOL if … else … then", "runs one branch"),
        "begin" | "until" => ("begin … BOOL until", "runs until the bool is true"),
        "define" | "end" => (
            "define NAME … end",
            "a word of your own; it may call itself (end also closes list and each)",
        ),
        "let" => (
            "X let NAME",
            "NAME pushes X from then on (in a definition, for that call)",
        ),
        "list" => (
            "list … end",
            "a list of the values its body leaves; the body starts with an empty stack",
        ),
        "each" => (
            "SEQ each … end",
            "the body once per element of SEQ, a string or list, the element pushed",
        ),
        "str" => ("str WORD", "WORD as text, whatever it looks like"),
        _ => return None,
    })
}

/// The words that build what a word takes, its options or its input: the
/// words each uses, which `explain` prints and the word refuses others of.
/// Words that wait take a wait's words, and `marks` for the screen it
/// prints.
pub(crate) const OPTIONS: [(&str, &str); 9] = [
    (
        "run",
        "name size cwd env command program arg in log-to id \
         wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
    (
        "split",
        "horizontal vertical cwd env command program arg typed id \
         wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
    ("new-tab", "name size cwd env typed in id"),
    ("new-workspace", "name size cwd env typed id"),
    (
        "respawn",
        "kill-first env command program arg wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
    (
        "capture",
        "history all-history joined full styles marks main-screen alt-screen last-output \
         rows cols json timeout",
    ),
    (
        "wait",
        "wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
    (
        "send-keys",
        "text key paste paste-file times wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
    (
        "send-mouse",
        "wait-text wait-gone wait-exit wait-bell timeout marks",
    ),
];

/// The options `name` takes, if it takes any.
pub(crate) fn options(name: &str) -> Option<std::str::SplitWhitespace<'static>> {
    (OPTIONS.iter())
        .find(|(word, _)| *word == name)
        .map(|(_, takes)| takes.split_whitespace())
}

/// An example of a word in use, as a command line.
pub(crate) fn example(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "fux opts str app name 120 40 size str ./app program str --flag arg run",
        "split" => "fux %3 opts vertical split",
        "command" => "fux opts str 'python3 -I app.py --fast' command run",
        "new-tab" => "fux opts str logs name str agent in new-tab",
        "new-workspace" => "fux opts str work name new-workspace",
        "respawn" => "fux %3 opts kill-first respawn",
        "wait" => "fux %3 opts wait",
        "wait-text" => "fux %3 opts str ready wait-text 30 timeout wait",
        "times" => "fux %3 keys str Down key 2 times str Enter key send-keys",
        "line" => "fux %3 str 'make test' line",
        "send-keys" => "fux %3 keys str 'ls -l' text str Enter key send-keys",
        "key" => "fux %3 keys str C-c key send-keys",
        "capture" => "fux %3 opts 100 history capture",
        "get" => "fux %3 str exited get",
        "json" => "fux ls json",
        "info" => "fux %3 info json",
        "dump" => "fux str work workspace dump-of > work.fux; fux work.fux",
        "rename" => "fux %3 str editor rename",
        "kill" => "fux %3 kill",
        "resize-x" => "fux %3 30 percent resize-x",
        "move-to" => "fux %3 str new-tab move-to",
        "focus" => "fux str c1 client str left focus",
        "bind" => "str 'g v' str 'me focused opts horizontal split drop' bind",
        "bind-root" => "str M-n str 'me next-tab' bind-root",
        "prefix" => "str C-a prefix",
        "set" => "fux str history-bytes 16777216 set",
        "server" => "fux str test server start opts str ./app program run",
        "headless" => "fux 100 30 headless",
        "grant" => "fux str agent workspace str /run/user/1000/agent.sock grant",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word, and every word of the library, has its help; examples
    /// compile.
    #[test]
    fn every_word_has_its_help() {
        let overview = overview();
        for word in WORDS {
            assert!(overview.contains(word.name), "{}", word.name);
            assert!(explain(word.name).is_some(), "{}", word.name);
            if let Some(example) = example(word.name) {
                let code = example.strip_prefix("fux ").unwrap_or(example);
                let code = code
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .split(" >")
                    .next()
                    .unwrap_or_default();
                let tokens = fuxth::token::source("example", code);
                let compiled = tokens.map_err(|e| e.to_string()).and_then(|t| {
                    crate::script::compile(&t)
                        .map(drop)
                        .map_err(|e| e.to_string())
                });
                assert!(compiled.is_ok(), "{}: {compiled:?}", word.name);
            }
        }
        assert!(library().len() >= 5);
        // A word defined over several lines shows all of them.
        let range = explain("range").unwrap_or_default();
        assert!(range.contains("  define range\n    let to\n"), "{range}");
        assert!(range.ends_with("\n  end\n"), "{range}");
        for (name, ..) in library() {
            assert!(explain(name).is_some(), "{name}");
        }
        // Each word's options go on what it takes.
        for (name, on) in [("send-keys", "after keys"), ("run", "after opts")] {
            let explained = explain(name).unwrap_or_default();
            assert!(explained.contains(&format!("options, {on}")), "{explained}");
        }
        // Each option is a word, which explain lists.
        for (name, takes) in OPTIONS {
            for option in takes.split_whitespace() {
                assert!(WORDS.iter().any(|w| w.name == option), "{name} {option}");
            }
        }
        assert!(AGENTS.contains("fux "));
    }
}
