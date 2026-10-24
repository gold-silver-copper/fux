//! fux's help, made from its words (`crate::script::WORDS`) and the bundled
//! library's comments, so that it always says what this binary does; and
//! the recipe for driving programs from a script or an agent
//! (`help-agents`), which is `docs/agents.md`, built in.
use crate::script::{LIBRARY, WORDS};

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");
/// The guide's first screen (`help-agents`): all before its first topic.
pub fn agents_first() -> &'static str {
    AGENTS
        .split_once("\n## ")
        .map_or(AGENTS, |(first, _)| first)
}

/// One topic of the guide (`help-agents-on`), its section headed `## TOPIC:`,
/// or `all` of it; `None` if it has none so named.
pub fn agents_topic(topic: &str) -> Option<String> {
    if topic == "all" {
        return Some(AGENTS.to_owned());
    }
    let heading = format!("\n## {topic}:");
    let (_, rest) = AGENTS.split_once(&heading)?;
    let section = rest
        .split_once("\n## ")
        .map_or(rest, |(section, _)| section);
    Some(format!("##{}{section}", heading.trim_start_matches("\n##")))
}

/// The guide's topics, as `help-agents-on` takes them.
pub fn agents_topics() -> Vec<&'static str> {
    (AGENTS.split("\n## ").skip(1))
        .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
        .collect()
}

/// What `json` gives, word by word (`help-json`).
pub const JSON: &str = include_str!("../docs/json.md");

/// How fux runs, and every word under its heading.
pub fn overview() -> String {
    let mut out = String::from(
        "\
usage: fux                 attach, starting a server if none runs
       fux WORD...         run the words, a fuxth program (or all in one argument)
       fux FILE            run a file of fuxth (a word with a / or ending .fux)
       fux -               run fuxth read from stdin

Words run left to right, each taking what the words before it left: a
number is a value; %2, @2 and +1 are a pane, a tab and a workspace; `str
WORD` pushes WORD as text. What is left is printed. What a word that acts
takes (options, keys, a version, what to read) goes on the pane before it
(`%2 str y text send-keys`, `%2 str 0.57.0.3 changed wait`, `%2
all-history capture`); only run, new-tab and new-workspace start from opts
(`opts str 'make test' command run`). An action answers at once, with its
pane's version before it acted; a wait waits for the version to differ,
30s at most (SECS timeout). A failure prints `fux: CODE: MESSAGE` on stderr.
Driving programs from a script or an agent: `fux help-agents`; one word:
`fux help WORD`.

The language: str WORD, define NAME … end, X let NAME, if … else … then,
begin … until, list … end, SEQ each … end; the core's words drop pick roll
add sub mul div lt eq len at slice concat push put code char kind string
print fail; and the libraries below, written in fuxth. Mistakes every run
would make are refused before anything runs.

Below, x is a pane, tab, workspace or client (an action's answer is its
pane), a request on one, or opts; target, subject and place a pane (or a
tab, workspace or client) or a request on one; handle a pane, tab or
workspace; ref a name or a handle; text a string, number or boolean.
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
    out.push_str("\nThe libraries (`fux help WORD` says each):\n ");
    let mut width = 1usize;
    for (name, ..) in library() {
        width = width.saturating_add(name.len()).saturating_add(1);
        if width > 78 {
            out.push_str("\n ");
            width = name.len().saturating_add(2);
        }
        out.push(' ');
        out.push_str(name);
    }
    out.push_str(
        "\n\nThe server: `str NAME server` or `str PATH socket` first; else\n\
         FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux.\n",
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
            // What they go on: options, for what makes its own pane.
            let on = match name {
                "run" | "new-tab" | "new-workspace" => "opts",
                _ => "the pane",
            };
            out.push_str(&format!("\nIts options, after {on} and before {name}:\n"));
            for option in takes {
                if let Some(w) = WORDS.iter().find(|w| w.name == option) {
                    out.push_str(&format!("  {:<14} {:<26} {}\n", w.name, w.effect, w.about));
                }
            }
        }
        return Some(out);
    }
    if let Some((name, effect, about, code)) = library().into_iter().find(|(n, ..)| *n == name) {
        return Some(format!(
            "{name}  ( {effect} )\n\n{about}.\nWritten in fuxth:\n\n  {code}\n"
        ));
    }
    if let Some(builtin) = fuxth::Builtin::named(name) {
        return Some(format!(
            "{name}  ( {} )\n\n{}. One of the core's words.\n",
            builtin.effect(),
            builtin.about()
        ));
    }
    let (shape, about) = fuxth::Parsing::named(name)?.syntax();
    Some(format!("{name}  {shape}: {about}\n"))
}

/// The words that build what a word takes, its options or its input: the
/// words each uses, which `explain` prints and the word refuses others of;
/// and whether it takes a wait's words too: the `Waiting` words that go on
/// the pane.
pub(crate) const OPTIONS: [(&str, &str, bool); 10] = [
    (
        "run",
        "name size cwd env command program arg in log-to id",
        false,
    ),
    (
        "split",
        "horizontal vertical cwd env command program arg typed id",
        false,
    ),
    ("new-tab", "name size cwd env typed in id", false),
    ("new-workspace", "name size cwd env typed id", false),
    ("respawn", "kill-first env command program arg", false),
    (
        "capture",
        "history all-history wrapped full styles marks main-screen alt-screen last-output \
         rows cols",
        false,
    ),
    ("wait", "", true),
    ("send-keys", "text key paste paste-file bytes times", false),
    ("send-mouse", "mouse", false),
    ("signal", "", false),
];

/// The options `name` takes, if it takes any: its own, then a wait's.
pub(crate) fn options(name: &str) -> Option<impl Iterator<Item = &'static str>> {
    let (_, takes, waits) = OPTIONS.iter().find(|(word, ..)| *word == name)?;
    let waiting = (WORDS.iter())
        .filter(|w| *waits && w.group == "Waiting" && w.effect.starts_with("x "))
        .map(|w| w.name);
    Some(takes.split_whitespace().chain(waiting))
}

/// An example of a word in use, as a command line.
pub(crate) fn example(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "fux opts str web name 120 40 size str ./app program str --flag arg run",
        "split" => "fux %3 vertical split",
        "command" => "fux opts str 'python3 -I app.py --fast' command run",
        "name" | "size" | "cwd" | "env" | "program" | "arg" | "in" | "log-to" => {
            "fux opts str web name 80 24 size str /srv cwd str agent in str ./app program run"
        }
        "typed" => "fux opts str 'make test' typed new-tab",
        "new-tab" => "fux opts str logs name str agent in new-tab",
        "new-workspace" => "fux opts str work name new-workspace",
        "respawn" => "fux %3 kill-first respawn",
        "wait" => "fux %3 str 0.57.0.3 changed 60 timeout wait",
        "help-agents-on" => "fux str waits help-agents-on",
        "changed" => "fux %3 str 0.57.0.3 changed wait",
        "timeout" => "fux %3 300 timeout wait",
        "times" => "fux %3 str Down key 2 times str Enter key send-keys",
        "bytes" => "fux %3 str 1b5b41 bytes send-keys",
        "line" => "fux %3 str 'make test' line",
        "send-keys" | "text" => "fux %3 str 'ls -l' text str Enter key send-keys",
        "key" => "fux %3 str C-c key send-keys",
        "mouse" | "send-mouse" => "fux %3 str click 2 10 mouse send-mouse",
        "capture" | "history" => "fux %3 100 history capture",
        "all-history" => "fux %3 all-history capture",
        "wrapped" => "fux %3 wrapped 0 2 rows capture",
        "full" => "fux %3 full capture",
        "styles" => "fux %3 styles capture",
        "main-screen" => "fux %3 main-screen capture",
        "alt-screen" => "fux %3 alt-screen capture",
        "last-output" => "fux %3 last-output capture",
        "rows" => "fux %3 0 4 rows capture",
        "cols" => "fux %3 0 39 cols capture",
        "get" => "fux %3 str exited get",
        "json" => "fux ls json",
        "info" => "fux %3 info json",
        "dump" => "fux str work workspace dump-of > work.fux; fux work.fux",
        "rename" => "fux %3 str editor rename",
        "kill" => "fux %3 kill",
        "resize-x" => "fux %3 30 percent resize-x",
        "move-to" => "fux %3 str new-tab move-to",
        "focus" => "fux str c1 client str left focus",
        "bind" => "str 'g v' str 'me focused horizontal split drop' bind",
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
        for (name, on) in [("send-keys", "after the pane"), ("run", "after opts")] {
            let explained = explain(name).unwrap_or_default();
            assert!(explained.contains(&format!("options, {on}")), "{explained}");
        }
        // Each option is a word, which explain lists.
        for (name, ..) in OPTIONS {
            for option in options(name).into_iter().flatten() {
                assert!(WORDS.iter().any(|w| w.name == option), "{name} {option}");
            }
        }
        assert!(AGENTS.contains("fux "));
    }
}
