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
word runs. What is left at the end is printed, a value a line. A failure
prints `fux: CODE: MESSAGE` on stderr, with how the word is used; a wait's
status is the exit status (3 exited, 4 closed, 5 unchanged, 6 changed, 124
timeout), any other failure 1. Each `fux` is a fresh program.

Most steps have one of these shapes (UPPER = what you fill in):

  fux opts OPTIONS run                  start a program; prints its pane, %2
  fux %2 keys INPUT [WAIT] send-keys    type: str T text, str Enter key, …
  fux %2 opts WAIT wait                 wait: str T wait-text, 300 wait-idle, …
  fux %2 opts READ capture              read: 100 history, marks, json, …
  fux %2 str FIELD get                  one value: exited, pid, revision, …

  fux opts str 'htop -d 5' command run  start htop
  fux %2 str Load await                 wait until it shows Load
  fux ls                                what there is

Language: dup drop swap over rot  add sub mul div mod  eq lt gt not
  if … else … then   begin … until   define NAME … end   constant NAME
  str WORD   # a comment to the end of the line
  Mistakes a run would surely make (too few values, a string where an
  integer goes) are found before anything runs.

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
    out.push_str("\nBundled words (written in fuxth: its prelude, then fux's library):\n");
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
/// `# pane text -- : what it does`.
fn library() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    let mut words = Vec::new();
    let mut comment = None;
    for line in fuxth::PRELUDE.lines().chain(LIBRARY.lines()) {
        if let Some(text) = line.strip_prefix("# ") {
            comment = text.split_once(" : ");
        } else if let Some(rest) = line.strip_prefix("define ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if let Some((effect, about)) = comment.take() {
                words.push((name, effect, about, line));
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
            out.push_str(&format!(
                "\nIts options, after opts (or keys) and before {name}:\n"
            ));
            for option in takes {
                if let Some(w) = WORDS.iter().find(|w| w.name == *option) {
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
        let about = match builtin {
            fuxth::Builtin::Add
            | fuxth::Builtin::Sub
            | fuxth::Builtin::Mul
            | fuxth::Builtin::Div
            | fuxth::Builtin::Mod => {
                ": integer arithmetic, checked; div truncates toward zero, and mod takes the sign of n"
            }
            fuxth::Builtin::Eq => ": whether two values are the same, of one kind",
            fuxth::Builtin::Dup
            | fuxth::Builtin::Drop
            | fuxth::Builtin::Swap
            | fuxth::Builtin::Over
            | fuxth::Builtin::Rot
            | fuxth::Builtin::Lt
            | fuxth::Builtin::Gt
            | fuxth::Builtin::Not => "",
        };
        return Some(format!("{name}  ( {} ){about}\n", builtin.effect()));
    }
    let language = match name {
        "if" | "else" | "then" => "bool if … else … then: runs one branch",
        "begin" | "until" => "begin … bool until: runs until the bool is true",
        "define" | "end" => "define NAME … end: a word of your own; it may call itself",
        "constant" => "x constant NAME: NAME pushes x from then on",
        "str" => "str WORD: WORD as text, whatever it looks like",
        _ => return None,
    };
    Some(format!("{name}  {language}\n"))
}

/// The words that build what a word takes: its options, or its input.
fn options(name: &str) -> Option<&'static [&'static str]> {
    const WAITS: [&str; 10] = [
        "wait-text",
        "wait-gone",
        "wait-idle",
        "wait-change",
        "wait-exit",
        "wait-done",
        "wait-bell",
        "timeout",
        "with-screen",
        "marks",
    ];
    Some(match name {
        "run" => &[
            "name",
            "size",
            "cwd",
            "env",
            "command",
            "program",
            "arg",
            "in",
            "log-to",
            "id",
            "wait-text",
            "wait-idle",
            "wait-exit",
            "timeout",
        ],
        "split" => &[
            "horizontal",
            "vertical",
            "cwd",
            "env",
            "command",
            "program",
            "arg",
            "typed",
            "id",
        ],
        "new-tab" | "new-workspace" => &["name", "size", "cwd", "env", "typed", "in", "id"],
        "respawn" => &[
            "kill-first",
            "env",
            "command",
            "program",
            "arg",
            "wait-text",
            "wait-exit",
            "timeout",
        ],
        "capture" => &[
            "history",
            "all-history",
            "joined",
            "full",
            "styles",
            "marks",
            "main-screen",
            "alt-screen",
            "last-output",
            "rows",
            "cols",
            "json",
        ],
        "wait" => &WAITS,
        "send-keys" => &[
            "text",
            "key",
            "paste",
            "paste-file",
            "times",
            "if-revision",
            "wait-text",
            "wait-idle",
            "wait-done",
            "wait-change",
            "timeout",
            "with-screen",
            "marks",
        ],
        "send-mouse" => &[
            "wait-text",
            "wait-idle",
            "wait-change",
            "timeout",
            "with-screen",
        ],
        _ => return None,
    })
}

/// An example of a word in use, as a command line.
pub(crate) fn example(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "fux opts str app name 120 40 size str ./app program str --flag arg run",
        "split" => "fux %3 opts vertical split",
        "command" => "fux opts str 'python3 -I app.py --fast' command run",
        "new-tab" => "fux opts str logs name new-tab",
        "new-workspace" => "fux opts str work name new-workspace",
        "respawn" => "fux %3 opts kill-first respawn",
        "wait-text" | "wait" => "fux %3 opts str ready wait-text 30 timeout wait",
        "wait-idle" => "fux %3 opts 500 wait-idle wait",
        "wait-done" => "fux %3 keys str make text str Enter key wait-done send-keys",
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
        "bind" => "str 'v h' str 'me focused opts horizontal split drop' bind",
        "bind-root" => "str M-n str 'me next-tab' bind-root",
        "prefix" => "str C-a prefix",
        "set" => "fux str history-lines 50000 set",
        "server" => "fux str test server start opts str ./app program run",
        "headless" => "fux 100 30 headless",
        "grant" => "fux str agent workspace str /run/user/1000/agent.sock grant",
        "if-revision" => "fux %3 keys 42 if-revision str y text send-keys",
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
        for (name, ..) in library() {
            assert!(explain(name).is_some(), "{name}");
        }
        assert!(AGENTS.contains("fux "));
    }
}
