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

fuxth is a small Forth: words run left to right, taking their values from
the stack and leaving theirs. A number is a value, any other bare word runs;
`str WORD` pushes WORD as text, however it looks. What is left at the end is printed,
a value a line. A failure prints `fux: CODE: MESSAGE` on stderr; a wait's
status is the exit status (3 exited, 4 closed, 5 unchanged, 6 changed,
124 timeout), any other failure 1.

  fux opts str htop program run                 start htop, print its pane
  fux str %3 pane str Load await                wait until a pane shows Load
  fux ls                                        what there is

Language: dup drop swap over  add sub mul div mod  eq lt gt not
  if … else … then   begin … until   define NAME … end   constant NAME
  str WORD   # a comment to the end of the line

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
    out.push_str("\nBundled words (fuxth, from the library):\n");
    for (name, effect, about) in library() {
        out.push_str(&format!("  {name:<20} {effect:<28} {about}\n"));
    }
    out.push_str(
        "\nThe server: `str NAME server` or `str PATH socket` before the rest;\n\
         else FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux. Inside a\n\
         pane, `here` is that pane (FUX_PANE).\n",
    );
    out
}

/// The library's words: each name, stack effect and what it does, from
/// the comment before its `define`: `# pane text -- : what it does`.
fn library() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut words = Vec::new();
    let mut comment = None;
    for line in LIBRARY.lines() {
        if let Some(text) = line.strip_prefix("# ") {
            comment = text.split_once(" : ");
        } else if let Some(rest) = line.strip_prefix("define ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if let Some((effect, about)) = comment.take() {
                words.push((name, effect, about));
            }
        }
    }
    words
}

/// One word at length: its stack effect, what it does, and an example.
pub fn explain(name: &str) -> Option<String> {
    if let Some(word) = WORDS.iter().find(|w| w.name == name) {
        let mut out = format!("{}  ( {} )\n\n{}.\n", word.name, word.effect, word.about);
        if let Some(example) = example(name) {
            out.push_str(&format!("\n  {example}\n"));
        }
        return Some(out);
    }
    if let Some((name, effect, about)) = library().into_iter().find(|(n, ..)| *n == name) {
        let code = LIBRARY
            .lines()
            .find(|l| {
                l.strip_prefix("define ")
                    .and_then(|r| r.split_whitespace().next())
                    == Some(name)
            })
            .unwrap_or_default();
        return Some(format!(
            "{name}  ( {effect} )\n\n{about}.\nFrom the library:\n\n  {code}\n"
        ));
    }
    let language = match name {
        "dup" => "x -- x x: a copy of the top value",
        "drop" => "x -- : the top value dropped",
        "swap" => "a b -- b a: the top two swapped",
        "over" => "a b -- a b a: a copy of the second",
        "add" | "sub" | "mul" | "div" | "mod" => "a b -- n: integer arithmetic, checked",
        "eq" | "lt" | "gt" => "a b -- bool: comparison",
        "not" => "bool -- bool",
        "if" | "else" | "then" => "bool if … else … then: runs one branch",
        "begin" | "until" => "begin … bool until: runs until the bool is true",
        "define" | "end" => "define NAME … end: a word of your own; it may call itself",
        "constant" => "x constant NAME: NAME pushes x from then on",
        "str" => "str WORD: WORD as text, whatever it looks like",
        _ => return None,
    };
    Some(format!("{name}  {language}\n"))
}

/// An example of a word in use, as a command line.
fn example(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "fux opts str app name 120 40 size str ./app program str --flag arg run",
        "split" => "fux here opts vertical split",
        "new-tab" => "fux opts str logs name new-tab",
        "new-workspace" => "fux opts str work name new-workspace",
        "respawn" => "fux str %3 pane opts kill-first respawn",
        "wait-text" | "wait" => "fux str %3 pane opts str ready wait-text 30 timeout wait",
        "wait-idle" => "fux str %3 pane opts 500 wait-idle wait",
        "wait-done" => "fux str %3 pane keys str make text str Enter key wait-done send-keys",
        "send-keys" => "fux str %3 pane keys str 'ls -l' text str Enter key send-keys",
        "key" => "fux str %3 pane keys str C-c key send-keys",
        "capture" => "fux str %3 pane opts 100 history capture",
        "get" => "fux str %3 pane str exited get",
        "json" => "fux ls json",
        "info" => "fux str %3 pane info json",
        "dump" => "fux dump > session.fux; fux session.fux",
        "rename" => "fux str %3 pane str editor rename",
        "kill" => "fux str %3 pane kill",
        "resize-x" => "fux str %3 pane 30 percent resize-x",
        "move-to" => "fux str %3 pane str new-tab move-to",
        "focus" => "fux me str left focus",
        "bind" => "str 'v h' str 'me focused opts horizontal split drop' bind",
        "bind-root" => "str M-n str 'me next-tab' bind-root",
        "prefix" => "str C-a prefix",
        "set" => "fux str history-lines 50000 set",
        "server" => "fux str test server start opts str ./app program run",
        "headless" => "fux 100 30 headless",
        "grant" => "fux str agent workspace str /run/user/1000/agent.sock grant",
        "if-revision" => "fux str %3 pane keys 42 if-revision str y text send-keys",
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
