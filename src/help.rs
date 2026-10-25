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
    let wait = crate::config::Limit::DEFAULT.seconds();
    let mut out = format!(
        "\
usage: fux                 attach, starting a server if none runs
       fux WORD...         run the words, a fuxth program (or all in one argument)
       fux FILE            run a file of fuxth (a word with a / or ending .fux)
       fux -               run fuxth read from stdin

Words run left to right, each taking what the words before it left: a
number is a value; %2, @2 and +1 are a pane, a tab and a workspace; `str
WORD` pushes WORD as text. What is left is printed. What a word that acts
takes (options, keys, what to read) goes on the pane before it (`%2 str y
text send-keys`, `%2 all-history capture`); only run, new-tab and
new-workspace start from opts (`opts str make program str test arg
run`), laid out on an attached client's screen, or with none (one that
only watches does not count) on a size given (`opts 80 24 size …`): fux
makes none up. An action answers at once,
with its pane's version from before it acted, one word
(`%2:0.57.0.3.0:RUN`); `str V changed wait` waits in V's pane until its
version differs from V, {wait}s at most unless `SECS timeout` says (the
server's wait-seconds). A failure prints `fux: CODE: MESSAGE` on stderr.
Driving programs from a script or an agent: `fux help-agents`; one word:
`fux help WORD`.
",
    );
    // The language's parsing words and core, as fuxth says them: a block's
    // words once, never broken across lines.
    let mut shapes: Vec<String> = Vec::new();
    for (shape, _) in fuxth::Parsing::ALL.map(fuxth::Parsing::syntax) {
        let unbroken = shape.replace(' ', "\u{a0}");
        if !shapes.contains(&unbroken) {
            shapes.push(unbroken);
        }
    }
    let core = fuxth::Builtin::ALL.map(fuxth::Builtin::name).join(" ");
    let language = format!(
        "The language: {}; the core's words {core}; and the libraries below, written in \
         fuxth. Mistakes every run would make are refused before anything runs.",
        shapes.join(", ")
    );
    let words = language.split(' ').map(|w| w.replace('\u{a0}', " "));
    out.push_str(&wrapped(words, "\n", 74));
    out.push_str(
        "

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
    out.push_str("\nThe libraries (`fux help WORD` says each):");
    let names = library().into_iter().map(|(name, ..)| name.to_owned());
    out.push_str(&wrapped(names, "\n  ", 79));
    out.push_str(
        "\n\nThe server: `str NAME server` or `str PATH socket` first; else\n\
         FUX_SOCKET; else server.sock in $XDG_RUNTIME_DIR/fux.\n",
    );
    out
}

/// `words` joined by blanks, in lines each begun with `start` and at most
/// `width` columns long, the line end included.
fn wrapped(words: impl Iterator<Item = String>, start: &str, width: usize) -> String {
    let (mut out, mut line) = (String::new(), String::from(start));
    for word in words {
        if line.len() > start.len() {
            if line.chars().count().saturating_add(word.chars().count()) >= width {
                out.push_str(&line);
                line = String::from(start);
            } else {
                line.push(' ');
            }
        }
        line.push_str(&word);
    }
    out + &line
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
        if let Some(mut takes) = options(name).map(Iterator::peekable)
            && takes.peek().is_some()
        {
            // What they go on: options, for what makes its own pane.
            let on = match FROM_OPTS.contains(&name) {
                true => "opts",
                false => "the pane",
            };
            out.push_str(&format!("\nIts options, after {on} and before {name}:\n"));
            for option in takes {
                if let Some(w) = WORDS.iter().find(|w| w.name == option) {
                    out.push_str(&format!("  {:<14} {:<26} {}\n", w.name, w.effect, w.about));
                }
            }
        }
        if name == "set" {
            out.push_str(&settings());
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

/// Every setting `set` takes: its name, what text it takes, its default,
/// and what it does, from `config`'s table.
fn settings() -> String {
    let mut out = String::from("\nThe settings:\n");
    for [name, takes, default, about] in crate::config::table() {
        let about = format!("{about} By default {default}.");
        let about = about.split_whitespace().map(str::to_owned);
        out.push_str(&format!(
            "  {name}: {takes}{}\n",
            wrapped(about, "\n      ", 78)
        ));
    }
    out
}

/// The words that start from `opts`, empty options: they make their own
/// pane. Every other word's options go on the pane it acts on.
pub(crate) const FROM_OPTS: [&str; 3] = ["run", "new-tab", "new-workspace"];

/// The words that build what a word takes, its options or its input: the
/// words each uses, which `explain` prints and the word refuses others of.
pub(crate) const OPTIONS: [(&str, &str); 10] = [
    ("run", "name size cwd env program arg in log-to id"),
    ("split", "horizontal vertical shares cwd env program arg id"),
    ("new-tab", "name size cwd env in id"),
    ("new-workspace", "name size cwd env id"),
    ("respawn", "kill-first env program arg"),
    (
        "capture",
        "history all-history wrapped full styles marks main-screen alt-screen last-output \
         rows cols",
    ),
    ("wait", "changed timeout"),
    ("send-keys", "text key paste bytes"),
    ("send-mouse", "mouse"),
    ("signal", ""),
];

/// The options `name` takes, if it takes any.
pub(crate) fn options(name: &str) -> Option<impl Iterator<Item = &'static str>> {
    let (_, takes) = OPTIONS.iter().find(|(word, _)| *word == name)?;
    Some(takes.split_whitespace())
}

/// An example of a word in use, as a command line.
pub(crate) fn example(name: &str) -> Option<&'static str> {
    Some(match name {
        "run" => "fux opts str web name 120 40 size str ./app program str --flag arg run",
        "split" => "fux %3 vertical split",
        "shares" => "fux %3 horizontal 2 1 shares split",
        "name" | "size" | "cwd" | "program" | "arg" | "in" | "log-to" => {
            "fux opts str web name 80 24 size str /srv cwd str agent in str ./app program str -v arg str app.log log-to run"
        }
        "env" => "fux opts str PORT 8080 env 80 24 size str ./app program run",
        "new-tab" => "fux opts str logs name 80 24 size str agent in new-tab",
        "new-workspace" => "fux opts str work name 80 24 size new-workspace",
        "respawn" | "kill-first" => "fux %3 kill-first respawn",
        "wait" | "changed" => {
            "A=$(fux %3 str y text send-keys); fux str \"$A\" changed 60 timeout wait"
        }
        "help-agents-on" => "fux str recipes help-agents-on",
        "timeout" => "fux %3 300 timeout wait",
        "bytes" => "fux %3 str 1b5b41 bytes send-keys",
        "line" => "fux %3 str 'make test' line",
        "send-keys" | "text" => "fux %3 str 'ls -l' text str Enter key send-keys",
        "key" => "fux %3 str C-c key send-keys",
        "paste" => "fux %3 str 'a long prompt' paste str Enter key send-keys",
        "signal" => "fux %3 str TERM signal",
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
        "ls" => "fux ls",
        "pane" => "fux str web pane capture",
        "tab" => "fux %3 tab str logs rename",
        "workspace" => "fux str agent workspace kill",
        "events-of" | "events-of-json" => "fux str agent workspace events-of-json",
        "clear-history" => "fux %3 clear-history",
        "notify" => "fux %3 str 'tests passed' notify",
        "status" => "fux status || fux start",
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
        "server" => "fux str test server start opts 80 24 size str ./app program run",
        "headless" => "fux 100 30 headless",
        "grant" => "fux str agent workspace str /run/user/1000/agent.sock grant",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the docs and words' help state a bound, they state the
    /// constant the check uses; the JSON they show is this protocol's.
    #[test]
    fn the_docs_state_each_bound_as_its_check_has_it() {
        // Thousands grouped by commas, as the docs write them.
        let grouped = |n: usize| {
            let digits = n.to_string();
            let mut out = String::new();
            for (i, digit) in digits.chars().enumerate() {
                if i > 0 && digits.len().saturating_sub(i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(digit);
            }
            out
        };
        let readme = include_str!("../README.md");
        let (least_width, least_height) = fux_vt::CellSize::LEAST;
        for said in [
            format!("{least_width} by {least_height} pixels"),
            format!("at most {} cells", grouped(crate::copy::MAX_CELLS)),
            format!("to {} MiB encoded", crate::copy::MAX_CLIPBOARD >> 20),
            format!("up to {} KiB encoded", fux_vt::OSC_PAYLOAD_LIMIT >> 10),
            format!("at most {} MiB", crate::protocol::MAX_FRAME >> 20),
            format!("past {} MiB", crate::pane::FRAME_LIMIT >> 20),
        ] {
            assert!(readme.contains(&said), "README: {said}");
        }
        let longest = format!("at most {} characters", grouped(fuxth::LONGEST));
        assert!(AGENTS.contains(&longest), "agents.md: {longest}");
        let protocol = crate::protocol::PROTOCOL;
        for said in [
            format!("\"protocol\":{protocol},"),
            format!("\"server_protocol\":{protocol},"),
            format!("its first {} characters", crate::pane::TITLE_MAX),
            format!("its first {} characters", crate::pane::NOTIFICATION_MAX),
            format!(
                "more than {} MiB on its terminal",
                crate::session::LEFT_MAX >> 20
            ),
            format!(
                "{} pixels a side, {} pixels in all",
                grouped(usize::try_from(fux_vt::sixel::MAX_SIDE).unwrap_or_default()),
                grouped(fux_vt::sixel::MAX_PIXELS)
            ),
        ] {
            assert!(JSON.contains(&said), "json.md: {said}");
        }
        let about = |name| WORDS.iter().find(|w| w.name == name).map(|w| w.about);
        let jump = format!("at most {} past the last", crate::id::MAX_JUMP);
        assert!(about("id").is_some_and(|a| a.contains(&jump)));
    }

    /// Every word, and every word of the library, has its help; examples
    /// compile.
    #[test]
    fn every_word_has_its_help() {
        let overview = overview();
        for word in WORDS {
            assert!(overview.contains(word.name), "{}", word.name);
            assert!(explain(word.name).is_some(), "{}", word.name);
            // Each fux command line in it, a shell line's too.
            let commands = (example(word.name).into_iter())
                .flat_map(|line| line.split(['(', ')', ';', '|', '>']))
                .filter_map(|part| part.trim().strip_prefix("fux "))
                .filter(|code| !code.ends_with(".fux"));
            for code in commands {
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
        // `set`'s help names every setting.
        let set = explain("set").unwrap_or_default();
        for name in crate::config::NAMES {
            assert!(set.contains(&format!("\n  {name}: ")), "{name}");
        }
    }
}
