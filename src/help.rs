//! fux's help, made from its words (`crate::script::Word`) and what fuxth
//! read of the libraries' words (`crate::script::docs`), so that it
//! always says what this binary does; and the guide to driving programs
//! from a script or an agent (`str agents explain`), which is
//! `docs/agents.md`, and the recipes (`str cookbook explain`), which are
//! `docs/cookbook.md`, built in.
use crate::script::{Act, After, Word};

/// `docs/agents.md`.
pub const AGENTS: &str = include_str!("../docs/agents.md");

/// `docs/cookbook.md`.
pub const COOKBOOK: &str = include_str!("../docs/cookbook.md");

/// Of the guide, `agents` its first screen (all before its first topic),
/// `guide` all of it, `cookbook` the recipes, else the topic's section,
/// headed `## TOPIC:`; `None` if it has none so named.
fn guide(topic: &str) -> Option<String> {
    let first = AGENTS
        .split_once("\n## ")
        .map_or(AGENTS, |(first, _)| first);
    match topic {
        "agents" => return Some(first.to_owned()),
        "guide" => return Some(AGENTS.to_owned()),
        "cookbook" => return Some(COOKBOOK.to_owned()),
        _ => {}
    }
    let heading = format!("\n## {topic}:");
    let (_, rest) = AGENTS.split_once(&heading)?;
    let section = rest
        .split_once("\n## ")
        .map_or(rest, |(section, _)| section);
    Some(format!("##{}{section}", heading.trim_start_matches("\n##")))
}

/// What `explain` says of a name that is neither a word nor a topic: the
/// topics, as the guide's headings name them.
pub fn unknown(name: &str) -> String {
    let topics = topics().join(" ");
    format!("no word or topic {name:?}; the topics: agents {topics} guide cookbook")
}

/// The guide's topics, as its headings name them.
fn topics() -> Vec<&'static str> {
    (AGENTS.split("\n## ").skip(1))
        .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
        .collect()
}

/// What `json` gives, word by word (`str json explain`).
pub const JSON: &str = include_str!("../docs/json.md");

/// How fux runs, and every word under its heading.
pub fn overview() -> String {
    let shebang = crate::script::SHEBANG;
    let from_opts = crate::script::from_opts("and");
    let mut out = format!(
        "\
usage: fux                     attach, starting a server if none runs
       fux WORD...             run the words, a fuxth program (or all in one argument)
       fux --file FILE ARG...  run a file of fuxth, its ARGs for `args` ({shebang})
       fux -                   run fuxth read from stdin

Words run left to right, each taking what the words before it left: a
number is a value; %2, @2 and +1 are a pane, a tab and a workspace; `str
WORD` pushes WORD as text. What is left is printed, a value a line (a
list an item a line, a list in it as JSON). What a word that acts
takes (options, keys, what to read) goes on the pane before it (`%2 str y
text send-keys`, `%2 all-history capture`); only
{from_opts} start from opts (`opts str jobs in str make
program str test arg new-tab`). A new pane is laid out on a size given
(`opts 80 24 size …`), else the caller's own screen or tab (`fux` run in
a pane): run from outside fux, a size must be given, as fux makes none
up. An action answers at once,
with its pane's version from before it acted, one word
(`%2:0.57.0.3.0:RUN`); `str V changed wait` waits in V's pane until its
version differs from V, for as long as that takes unless `SECS timeout`
bounds it, answering `timeout`. A failure prints nothing more on stdout:
`fux: CODE: MESSAGE` on stderr, then what was left. Exit statuses:
  0    done, or what `N exit` gave
  1    refused: anything else that failed
  2    a mistake in the program: usage, unknown_word, open_quote,
       missing_token, unmatched, unclosed, not_top_level, bad_codes,
       require, type, stack_underflow
  4    closed: the pane waited on closed
  124  timeout: a wait's limit came first
  141  stdout's reader went (a closed pipe), said nowhere
Driving programs from a script or an agent: `fux help agents`; one word:
`fux help WORD`; recipes to copy: `fux help cookbook`.
",
    );
    // The language's core, as fuxth says it: the words that shape a
    // program first.
    let core = |immediate: bool| {
        let core = fuxth::Builtin::ALL.into_iter().filter(|b| !b.hosted());
        let core = core
            .filter(|b| b.immediate() == immediate)
            .map(fuxth::Builtin::name);
        core.collect::<Vec<_>>().join(" ")
    };
    let language = format!(
        "The language: the words that shape a program, {}, which run as they are \
         read; the core's other words {}; and the libraries below, written in \
         fuxth. A word is found as it is read, a definition from its end on. Each \
         word checks its values as it runs, and fails there if it has too few or \
         one of a kind it doesn't take; the words before it have already acted.",
        core(true),
        core(false)
    );
    let words = language.split(' ').map(str::to_owned);
    out.push_str(&wrapped(words, "\n", 74));
    out.push_str(
        "

Effects read `inputs -- outputs`; a request must reach an action.
wait and send-keys leave an answer for a pane, and answers for a tab or
workspace (send-keys: or a client), which no word that wants a pane takes.
",
    );
    let names = crate::script::NAMES
        .iter()
        .map(|(name, said)| format!("{name}: {said}."))
        .collect::<Vec<_>>()
        .join(" ");
    out.push_str(&wrapped(names.split(' ').map(str::to_owned), "\n", 74));
    out.push('\n');
    let mut rows: Vec<_> = Word::ALL.iter().map(|w| w.row()).collect();
    rows.sort_by_key(|row| row.group);
    let mut group = None;
    for word in rows {
        if group != Some(word.group) {
            group = Some(word.group);
            out.push_str(&format!("\n{}:\n", word.group.name()));
        }
        out.push_str(&format!(
            "  {:<20} {:<28} {}\n",
            word.name, word.effect, word.about
        ));
    }
    out.push_str("\nThe libraries (`fux help WORD` says each):");
    let docs = crate::script::docs();
    out.push_str(&wrapped(
        docs.iter().map(|doc| doc.name.clone()),
        "\n  ",
        79,
    ));
    out.push_str(
        "\n\nThe server: FUX_SOCKET's socket (`FUX_SOCKET=/abs/path fux …`); else\n\
         server.sock in $XDG_RUNTIME_DIR/fux, or with XDG_RUNTIME_DIR unset\n\
         $TMPDIR/fux. Nothing is saved on its own:\n\
         `fux all dump > FILE` saves the layouts and launches when you choose\n\
         (never what a process held), and `fux start && fux --file FILE` makes\n\
         them again on a fresh server.\n",
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

/// One word at length, or a topic of the guide.
pub fn explain(name: &str) -> Option<String> {
    guide(name).or_else(|| word(name))
}

/// One word at length: its stack effect, what it does, and an example.
fn word(name: &str) -> Option<String> {
    if let Some(word) = Word::named(name) {
        let row = word.row();
        let mut out = format!("{}  ( {} )\n\n{}.\n", row.name, row.effect, row.about);
        if let Some(usage) = crate::script::usage(name) {
            out.push_str(&format!("\n{usage}\n"));
        }
        if let Word::Act(act) = word
            && let Some(takes) = act.takes()
        {
            // What they go on: options, for what makes its own pane.
            let on = match takes.after {
                After::Opts => "opts",
                After::Pane => "the pane",
                After::Either => "the pane (or opts)",
            };
            out.push_str(&format!("\nIts options, after {on} and before {name}:\n"));
            for option in takes.options.iter().map(|o| Word::Opt(*o).row()) {
                let (name, effect, about) = (option.name, option.effect, option.about);
                out.push_str(&format!("  {name:<14} {effect:<26} {about}\n"));
            }
        }
        if word == Word::Act(Act::Set) {
            out.push_str(&settings());
        } else if word == Word::Act(Act::Json) {
            out.push_str(&format!("\n{JSON}"));
        }
        return Some(out);
    }
    if let Some(doc) = crate::script::docs().iter().find(|doc| doc.name == name) {
        let (effect, about, code) = (&doc.effect, &doc.about, doc.code.replace('\n', "\n  "));
        return Some(format!(
            "{name}  ( {effect} )\n\n{about}.\nWritten in fuxth:\n\n  {code}\n"
        ));
    }
    let builtin = fuxth::Builtin::named(name).filter(|b| !b.hosted())?;
    let runs = match builtin.immediate() {
        true => " It runs as it is read, even while a definition is compiled.",
        false => "",
    };
    Some(format!(
        "{name}  ( {} )\n\n{}. One of the core's words.{runs}\n",
        builtin.effect(),
        builtin.about()
    ))
}

/// Local settings and their bundled initialization source.
fn settings() -> String {
    let mut out = String::from("\nThe settings:\n");
    for [name, takes, about] in crate::client_state::table() {
        let about = about.split_whitespace().map(str::to_owned);
        out.push_str(&format!(
            "  {name}: {takes}{}\n",
            wrapped(about, "\n      ", 78)
        ));
    }
    out.push_str("\nShell, history-bytes, program-clipboard, reflow,\nalternate-scroll-lines and picture-bytes supply this client's future launches.\nUse PANE BYTES history-budget to change one pane.\nBell, titles, terminal-clipboard, cell-placement, styles, bar, bar-slots\nand show-column affect only this client's view: supplied with attach, then updated after local commits\nbefore following requests.\nEscape-delay-ms and paste-timeout-ms change at an idle decoder boundary.\nJournal-entries sizes the journal of a server this client starts (start,\nor an attach that finds none); a running server keeps its own.\nSetters and config evaluation send no requests.\n");
    out.push_str("\nStyles rows: [role foreground background flags...]; roles: bar tab keys\nnotice error panel border border-focus. Colours: 0..255, default, #rrggbb.\nFlags: bold dim italic underline. A subset changes only its supplied roles.\nBar: [top/bottom/off [left fields] [right fields] tab-numbers?].\nFields: workspace tabs pane [slot 0..15]; each once.\nNative notices, prefix/repeat/zoom hints and clipping remain authoritative.\nOff, the default, takes no row: the bar overlays the bottom pane row while\na screen shows (the prefix's too), other feedback its corner; no PTY resizes.\nBar-slots replaces all slots: [0..15 text] rows, each slot once, text\nwith no controls. Text is inert. No paint-time programs or added timers.\nstr show-column false set hides the body, retaining dispatch, Escape and feedback.\n");
    out.push_str("\nBundled initialization (client-defaults.fux):\n");
    out.push_str(crate::script::DEFAULTS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Opt;

    /// The JSON guide shows this protocol's JSON, and names the language's
    /// codes fux's words raise.
    #[test]
    fn the_json_guide_states_this_protocol_and_its_codes() {
        let protocol = fux_wire::protocol::PROTOCOL;
        for said in [
            format!("\"protocol\":{protocol},"),
            format!("\"server_protocol\":{protocol},"),
            // The language's codes fux's words raise, which a try catches
            // only as fux says so.
            {
                let raised = fux_wire::code::Raise::LANG.iter();
                let declared = raised.filter(|c| !c.raised());
                let quoted: Vec<String> = declared.map(|c| format!("`{}`", c.name())).collect();
                let quoted: Vec<&str> = quoted.iter().map(String::as_str).collect();
                fux_wire::words::listed(&quoted, "and")
            },
        ] {
            assert!(JSON.contains(&said), "json.md: {said}");
        }
    }

    /// The guide's topics, as `explain` and the guide's first screen
    /// list them, are its headings, which `explain` reads; none is a word.
    /// The cookbook is a topic too, linked from there and from `fux help`.
    #[test]
    fn the_guide_lists_its_topics_as_its_headings_are() {
        let topics = super::topics();
        assert!(topics.len() >= 5, "{topics:?}");
        let on = Word::Act(Act::Explain).row().about;
        let listed = format!("({})", topics.join(" "));
        assert!(on.contains(&listed), "{listed}");
        for topic in topics.iter().chain(&["agents", "guide"]) {
            assert!(word(topic).is_none(), "{topic} is a word");
            assert!(
                explain(topic).is_some_and(|text| AGENTS.contains(&text)),
                "{topic}"
            );
        }
        let first = explain("agents").unwrap_or_default();
        let advertised = first.split_once("More:").map_or("", |(_, links)| links);
        assert!(explain("cookbook").is_some_and(|text| text == COOKBOOK));
        assert!(overview().contains("`fux help cookbook`"));
        for topic in topics.iter().chain(&["guide", "cookbook"]) {
            let link = format!("`{topic}`");
            let command = format!("`fux help {topic}`");
            assert!(
                advertised.contains(&link) || advertised.contains(&command),
                "topic links omit {topic}"
            );
        }
    }

    /// Every word, and every word of the library, has its help; examples
    /// compile.
    #[test]
    fn every_word_has_its_help() {
        let overview = overview();
        for word in Word::ALL.iter().map(|w| w.row()) {
            assert!(overview.contains(word.name), "{}", word.name);
            assert!(explain(word.name).is_some(), "{}", word.name);
            // Each fux command line in it, a shell line's too.
            let commands = (word.example.split(['(', ')', ';', '|', '>']))
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
        let docs = crate::script::docs();
        assert!(docs.len() >= 5);
        // A word defined over several lines shows all of them.
        let range = explain("range").unwrap_or_default();
        assert!(range.contains("  define range\n    let limit\n"), "{range}");
        assert!(range.ends_with("\n  end\n"), "{range}");
        for doc in docs.iter() {
            assert!(explain(&doc.name).is_some(), "{}", doc.name);
        }
        // Each word's options go on what it takes.
        for (name, on) in [("send-keys", "after the pane"), ("new-tab", "after opts")] {
            let explained = explain(name).unwrap_or_default();
            assert!(explained.contains(&format!("options, {on}")), "{explained}");
        }
        // A word's options go after opts just when it starts from them.
        for act in crate::script::Act::ALL {
            let (opts, row) = (act.takes().map(|t| t.after), Word::Act(*act).row());
            let from_opts = row.effect.starts_with("opts ");
            assert_eq!(opts == Some(After::Opts), from_opts, "{act}");
        }
        // Opts' own help names the words that start from them.
        let from_opts = crate::script::from_opts("and");
        assert!(Word::Act(Act::Opts).row().about.contains(&from_opts));
        assert!(overview.contains(&from_opts));
        // Each option goes on a word.
        for opt in Opt::ALL {
            assert!(!opt.takers().is_empty(), "{opt}");
        }
        assert!(AGENTS.contains("fux "));
        // `set`'s help names every setting.
        let set = explain("set").unwrap_or_default();
        for name in crate::client_state::NAMES {
            assert!(set.contains(&format!("\n  {name}: ")), "{name}");
        }
    }
}
