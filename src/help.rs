//! Help from the same native operation table that executes commands.
use crate::script::{After, Word};
pub const AGENTS: &str = include_str!("../docs/agents.md");
pub const DATA: &str = include_str!("../docs/data.md");
pub const COOKBOOK: &str = include_str!("../docs/cookbook.md");
pub const RECORDS: &str = include_str!("../docs/records.md");
const LANGUAGE: &str = "https://github.com/gold-silver-copper/grift/blob/pure/docs/LANGUAGE.md";
pub fn overview() -> String {
    let from_opts = crate::script::from_opts("and");
    let mut text = include_str!("help-usage.txt").replace("FROM_OPTIONS", &from_opts);
    let names = crate::script::language().unwrap_or_default();
    let (rows, rest) = names.as_chunks::<6>();
    for row in rows.iter().map(<[String; 6]>::as_slice).chain([rest]) {
        text.push_str(&format!("  {}\n", row.join(" ")));
    }
    text.push_str(include_str!("help-columns.txt"));
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
        text.push_str(&format!(
            "  {:<22} {:<28} {}\n",
            row.name, row.effect, row.about
        ));
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

/// What holds for every program: how one fails, and where its server is.
fn how_it_runs() -> String {
    include_str!("help-running.txt")
        .replace("SHEBANG", crate::script::SHEBANG)
        .replace("SOCKET", fux_server::socket::SOCKET_HELP)
}

/// Every setting `set` takes, what each takes and does, and the bundled
/// values.
fn settings(out: &mut String) {
    out.push_str("\nThe settings:\n");
    for [name, takes, about] in crate::client_state::table() {
        let about = about.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!("  {name}: {takes}\n      {about}\n"));
    }
    out.push_str(include_str!("help-settings.txt"));
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
    names
        .iter()
        .any(|known| known == name)
        .then(|| format!("{name}\nOne of grift's own names: {LANGUAGE} says what it does.\n"))
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
    use super::unbound::unbound;
    use super::*;
    use crate::script::{Act, Opt};

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
        for (name, on) in [
            ("send-keys", "around the pane"),
            ("new-tab", "around (options)"),
        ] {
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
