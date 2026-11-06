//! Help from the same native operation table that executes commands.
use crate::script::{After, Word};
pub const AGENTS: &str = include_str!("../docs/agents.md");
pub const DATA: &str = include_str!("../docs/data.md");
pub const COOKBOOK: &str = include_str!("../docs/cookbook.md");
pub const RECORDS: &str = include_str!("../docs/records.md");
const LANGUAGE: &str = "https://github.com/gold-silver-copper/grift/blob/pure/docs/LANGUAGE.md";
pub fn overview() -> String {
    let from_opts = crate::script::from_opts("and");
    let mut text = format!(
        "fux: a terminal multiplexer configured and driven in grift

usage: fux '(operation arguments...)' STRING...
       fux --file FILE STRING...
       fux -
       fux                 attach this terminal

A command line is one quoted grift program. Following shell arguments are strings
from (arguments). Records print as grift data; ask for fields with (get record \"field\").
Example: fux '(split (axis (pane 3) \"horizontal\"))'
Example: fux '(line (pane 3) \"ls -l\")'

What an operation takes (options, keys, what to read) goes around the pane it
acts on: `(send-keys (text (pane 2) \"y\"))`, `(capture (all-history (pane 2)))`;
only {from_opts} start from (options):
`(new-tab (argument (program (in (options) \"jobs\") \"make\") \"test\"))`. A new pane
is laid out on a size given (`(size (options) 80 24)`), else the caller's own
screen or tab (fux run in a pane): run from outside fux, a size must be given,
as fux makes none up. An action answers at once, with its pane's version from
before it acted (`%2:0.57.0.3.0:RUN`); `(wait (changed V))` waits in V's pane
until its version differs from V, for as long as that takes unless
`(wait (timeout (changed V) SECS))` bounds it, failing `timeout`.

The language is grift. `(explain NAME)` says where each of its own names is
described:
",
    );
    let language = crate::script::language().unwrap_or_default();
    text.push_str(&wrapped(language.iter().cloned(), "  ", 79));
    text.push_str(
        "

Each operation's column reads `takes -- gives`; a request must reach an action.
wait and send-keys leave an answer for a pane, and answers for a tab or
workspace (send-keys: or a client), which no word that wants a pane takes.
",
    );
    for (name, said) in crate::script::NAMES {
        text.push_str(&format!("  {name}: {said}.\n"));
    }
    let mut rows: Vec<_> = Word::ALL.iter().map(|word| word.row()).collect();
    rows.sort_by_key(|row| row.group);
    let mut group = None;
    for row in rows {
        if group != Some(row.group) {
            group = Some(row.group);
            text.push_str(&format!("\n{}:\n", row.group.name()));
        }
        let (name, effect, about) = (row.name, row.effect, row.about);
        text.push_str(&format!("  {name:<22} {effect:<28} {about}\n"));
    }
    if let Ok(docs) = crate::script::docs() {
        text.push_str("\nLibrary operations:\n");
        for doc in docs {
            text.push_str(&format!("  {} {}\n", doc.name, doc.parameters));
        }
    }
    text.push_str(&how_it_runs());
    text
}

/// `words` joined by blanks, in lines each begun with `start` and at most
/// `width` columns long, the line end included.
fn wrapped(words: impl Iterator<Item = String>, start: &str, width: usize) -> String {
    let (mut out, mut line) = (String::new(), String::from(start));
    for word in words {
        if line.len() > start.len() {
            if line.chars().count().saturating_add(word.chars().count()) >= width {
                out.push_str(&line);
                out.push('\n');
                line = String::from(start);
            } else {
                line.push(' ');
            }
        }
        line.push_str(&word);
    }
    out + &line
}

/// What holds for every program: how one fails, and where its server is.
fn how_it_runs() -> String {
    let shebang = crate::script::SHEBANG;
    let socket = fux_server::socket::SOCKET_HELP;
    format!(
        "
A file of grift runs with `fux --file FILE`, or as a script ({shebang}).
A file is read whole before it runs: one whose later form doesn't read runs
none of its forms.

A failure prints nothing more on stdout: `fux: KIND: MESSAGE` on stderr, each
action that already took effect after it, as `fux: already took effect: (CALL)`.
A program isn't undone: what ran before its failure stays done. Exit statuses:
  0    done, or what (exit N) gave
  1    refused: anything else that failed
  2    a mistake in the program
  4    closed: the pane waited on closed
  124  timeout: a wait's limit came first
  141  stdout's reader went (a closed pipe), said nowhere

The server: FUX_SOCKET's socket {socket} Nothing is saved on its own:
`fux '(dump (all))' > FILE` saves the layouts and launches when you choose
(never what a process held), and `fux '(start)'`, then `fux --file FILE`, makes
them again on a fresh server.

Driving programs from a script or an agent: `fux '(explain \"agents\")'`; one
operation: `fux '(explain \"split\")'`; recipes: `fux '(explain \"cookbook\")'`.
"
    )
}

/// Every setting `set` takes, what each takes and does, and the bundled
/// values.
fn settings(out: &mut String) {
    out.push_str("\nThe settings:\n");
    for [name, takes, about] in crate::client_state::table() {
        let about = about.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!("  {name}: {takes}\n      {about}\n"));
    }
    out.push_str("
Shell, history-bytes, program-clipboard, reflow, alternate-scroll-lines and
picture-bytes supply this client's future launches. Use (history-budget PANE
BYTES) to change one pane. Bell, titles, terminal-clipboard, cell-placement,
styles, bar, bar-slots and show-column affect only this client's view: supplied
with attach, then updated after local commits before following requests.
Escape-delay-ms and paste-timeout-ms change at an idle decoder boundary.
Journal-entries sizes the journal of a server this client starts ((start), or
an attach that finds none); a running server keeps its own. Setters and config
evaluation send no requests.

Styles rows: (role foreground background flags...); roles: bar tab keys notice
error panel border border-focus. Colours: 0..255, default, #rrggbb. Flags: bold
dim italic underline. A subset changes only its supplied roles.
Bar: (top/bottom/off (left fields) (right fields) tab-numbers?). Fields:
workspace tabs pane (slot 0..15); each once. Native notices, prefix/repeat/zoom
hints and clipping remain authoritative. Off, the default, takes no row: the bar
overlays the bottom pane row while a screen shows (the prefix's too), other
feedback its corner; no PTY resizes. Bar-slots replaces all slots: (0..15 text)
rows, each slot once, text with no controls. Text is inert. No paint-time
programs or added timers. (set \"show-column\" #false) hides the body, retaining
dispatch, Escape and feedback.
");
    out.push_str("\nBundled initialization (client-defaults.grift):\n");
    out.push_str(crate::script::DEFAULTS);
}

/// What `explain` says of a name that is neither a word nor a topic: the
/// topics, as the guide's headings name them.
pub fn unknown(name: &str) -> String {
    let topics = topics().join(" ");
    format!(
        "no operation or topic {name:?}; the topics: agents {topics} guide cookbook data records"
    )
}

/// The guide's topics, as its headings name them.
fn topics() -> Vec<&'static str> {
    (AGENTS.split("\n## ").skip(1))
        .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
        .collect()
}
pub fn explain(name: &str) -> Option<String> {
    if let Some(word) = Word::named(name) {
        let row = word.row();
        let mut text = format!(
            "{}\n{}\nArguments: {}\nExample: {}\n",
            row.name,
            row.about,
            row.effect
                .split_once("--")
                .map_or(row.effect, |(args, _)| args.trim()),
            row.example
        );
        if let Word::Act(act) = word
            && let Some(takes) = act.takes()
        {
            let on = match takes.after {
                After::Opts => "(options)",
                After::Pane => "the pane",
                After::Either => "the pane (or (options))",
            };
            text.push_str(&format!("\nIts options, around {on}:\n"));
            for option in takes.options.iter().map(|o| Word::Opt(*o).row()) {
                let (name, effect, about) = (option.name, option.effect, option.about);
                text.push_str(&format!("  {name:<14} {effect:<26} {about}\n"));
            }
        }
        if name == "set" {
            settings(&mut text);
        }
        return Some(text);
    }
    if let Ok(docs) = crate::script::docs()
        && let Some(doc) = docs.iter().find(|doc| doc.name == name)
    {
        return Some(format!(
            "{}\nParameters: {}\n{}\n{}\n",
            doc.name, doc.parameters, doc.documentation, doc.source
        ));
    }
    guide(name).or_else(|| language(name))
}

/// What help says of a name of grift's, or of what fux adds to it.
fn language(name: &str) -> Option<String> {
    if let Some((_, about)) = crate::script::HOST.iter().find(|(host, _)| *host == name) {
        return Some(format!("{name}\n{about}\n"));
    }
    let names = crate::script::language().ok()?;
    names.iter().any(|known| known == name).then(|| {
        format!("{name}\nOne of grift's own names: {LANGUAGE} says what it does.\n")
    })
}
fn guide(name: &str) -> Option<String> {
    match name {
        "agents" => {
            return Some(
                AGENTS
                    .split_once("\n## ")
                    .map_or(AGENTS, |(page, _)| page)
                    .to_owned(),
            );
        }
        "guide" => return Some(AGENTS.to_owned()),
        "cookbook" => return Some(COOKBOOK.to_owned()),
        "data" => return Some(DATA.to_owned()),
        "records" => return Some(RECORDS.to_owned()),
        _ => {}
    }
    let heading = format!("\n## {name}:");
    let (_, section) = AGENTS.split_once(&heading)?;
    let section = section
        .split_once("\n## ")
        .map_or(section, |(section, _)| section);
    Some(format!("## {name}:{section}"))
}
#[cfg(test)]
#[path = "../tests/support/unbound.rs"]
mod unbound;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{Act, Opt};
    use super::unbound::unbound;

    /// The names any program sees: fux's, its libraries' and grift's.
    fn known(name: &str) -> bool {
        Word::named(name).is_some()
            || crate::script::docs().is_ok_and(|docs| docs.iter().any(|doc| doc.name == name))
            || crate::script::language().is_ok_and(|names| names.iter().any(|n| n == name))
            || crate::script::HOST.iter().any(|(host, _)| *host == name)
    }

    /// The programs in a word's example: each one a shell line runs, else
    /// the example itself, a config's or a shell's own.
    fn programs(example: &str) -> Vec<&str> {
        let quoted = example.split("fux '").skip(1);
        let shell = quoted.filter_map(|rest| rest.split_once('\'').map(|(code, _)| code));
        let shell: Vec<&str> = shell.collect();
        if shell.is_empty() {
            vec![example]
        } else {
            shell
        }
    }

    /// Every word, and every word of the library, has its help; examples
    /// read, and every name in them is bound.
    #[test]
    fn every_word_has_its_help() -> Result<(), String> {
        let overview = overview();
        for word in Word::ALL.iter().map(|w| w.row()) {
            assert!(overview.contains(word.name), "{}", word.name);
            assert!(explain(word.name).is_some(), "{}", word.name);
            for program in programs(word.example) {
                let unbound = unbound(program, &known)?;
                assert!(unbound.is_empty(), "{}: {unbound:?}", word.name);
                // A word named bare does nothing: an example calls it.
                assert!(program.starts_with('('), "{}: {program}", word.name);
            }
        }
        let docs = crate::script::docs().map_err(|e| e.to_string())?;
        assert!(docs.len() >= 5);
        for doc in docs {
            assert!(explain(&doc.name).is_some(), "{}", doc.name);
        }
        // Each word's options go on what it takes.
        for (name, on) in [("send-keys", "around the pane"), ("new-tab", "around (options)")] {
            let explained = explain(name).unwrap_or_default();
            assert!(explained.contains(&format!("options, {on}")), "{explained}");
        }
        // Every option a word takes is under it.
        for act in Act::ALL {
            let explained = explain(Word::Act(*act).row().name).unwrap_or_default();
            for opt in act.takes().map_or(&[][..], |takes| takes.options) {
                let name = Word::Opt(*opt).row().name;
                assert!(explained.contains(&format!("\n  {name} ")), "{act}: {name}");
            }
        }
        // A word's options go on options just when it starts from them.
        for act in Act::ALL {
            let (opts, row) = (act.takes().map(|t| t.after), Word::Act(*act).row());
            let from_opts = row.effect.starts_with("options ");
            assert_eq!(opts == Some(After::Opts), from_opts, "{act}");
        }
        // The help of options names the words that start from them.
        let from_opts = crate::script::from_opts("and");
        assert!(Word::Act(Act::Opts).row().about.contains(&from_opts));
        assert!(overview.contains(&from_opts));
        // Each option goes on a word.
        for opt in Opt::ALL {
            assert!(!opt.takers().is_empty(), "{opt}");
        }
        assert!(AGENTS.contains("fux "));
        // The headings the words go under, what their columns mean, and
        // the language's own names.
        for row in Word::ALL.iter().map(|w| w.row()) {
            assert!(overview.contains(&format!("\n{}:\n", row.group.name())));
        }
        for (name, said) in crate::script::NAMES {
            assert!(overview.contains(&format!("{name}: {said}.")), "{name}");
        }
        let first = explain("first").unwrap_or_default();
        assert!(first.contains("LANGUAGE.md"), "{first}");
        Ok(())
    }
    /// D-25, and a file that doesn't read, are told where every word is.
    #[test]
    fn help_says_what_a_failure_leaves_done() {
        let help = overview();
        assert!(help.contains("already took effect"));
        assert!(help.contains("runs\nnone of its forms"));
    }
    #[test]
    fn set_lists_every_setting() {
        let set = explain("set").unwrap_or_default();
        for [name, ..] in crate::client_state::table() {
            assert!(set.contains(&format!("\n  {name}:")), "{name}");
        }
    }
    #[test]
    fn the_agent_page_fits_in_100_lines() {
        let page = explain("agents").unwrap_or_default();
        assert!(!page.is_empty());
        assert!(page.lines().count() <= 100);
    }
    /// The guide's topics, as `explain` and the guide's first screen list
    /// them, are its headings, which `explain` reads; none is a word. The
    /// cookbook is a topic too, linked from there and from `(help)`.
    #[test]
    fn the_guide_lists_its_topics_as_its_headings_are() {
        let topics = super::topics();
        assert!(topics.len() >= 5, "{topics:?}");
        let on = Word::Act(Act::Explain).row().about;
        let listed = format!("({})", topics.join(" "));
        assert!(on.contains(&listed), "{listed}");
        for topic in topics.iter().chain(&["agents", "guide"]) {
            assert!(Word::named(topic).is_none(), "{topic} is a word");
            assert!(
                explain(topic).is_some_and(|text| AGENTS.contains(&text)),
                "{topic}"
            );
        }
        let first = explain("agents").unwrap_or_default();
        let advertised = first.split_once("More:").map_or("", |(_, links)| links);
        assert!(explain("cookbook").is_some_and(|text| text == COOKBOOK));
        assert!(overview().contains(r#"(explain "cookbook")"#));
        for topic in topics.iter().chain(&["guide", "cookbook"]) {
            let link = format!("`{topic}`");
            let command = format!("`(explain \"{topic}\")`");
            assert!(
                advertised.contains(&link) || advertised.contains(&command),
                "topic links omit {topic}"
            );
        }
    }
    #[test]
    fn the_data_guide_states_this_protocol_and_its_codes() {
        assert!(DATA.contains(&format!("protocol {}", fux_wire::protocol::PROTOCOL)));
        for code in fux_wire::code::Code::all() {
            assert!(DATA.contains(&format!("`{code}`")), "{code}");
        }
    }
}
