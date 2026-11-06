//! Help from the same native operation table that executes commands.
use crate::script::Word;
pub const AGENTS: &str = include_str!("../docs/agents.md");
pub const DATA: &str = include_str!("../docs/data.md");
pub const COOKBOOK: &str = include_str!("../docs/cookbook.md");
pub fn overview() -> String {
    let mut text = String::from(
        "fux: a terminal multiplexer configured and driven in grift\n\nusage: fux '(operation arguments...)' STRING...\n       fux --file FILE STRING...\n       fux -\n       fux                 attach this terminal\n\nA command line is one quoted grift program. Following shell arguments are strings\nfrom (arguments). Records print as grift data; ask for fields with (get record \"field\").\nExample: fux '(split (axis (pane 3) \"horizontal\"))'\nExample: fux '(line (pane 3) \"ls -l\")'\n\nOperations:\n",
    );
    for word in Word::ALL {
        let row = word.row();
        if row.name != "json" {
            text += &format!("  {:<22} {}\n", row.name, row.about);
        }
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

/// What `explain` says of a name that is neither a word nor a topic: the
/// topics, as the guide's headings name them.
pub fn unknown(name: &str) -> String {
    let topics: Vec<&str> = (AGENTS.split("\n## ").skip(1))
        .filter_map(|section| section.split_once(':').map(|(topic, _)| topic))
        .collect();
    let topics = topics.join(" ");
    format!("no operation or topic {name:?}; the topics: agents {topics} guide cookbook data")
}
pub fn explain(name: &str) -> Option<String> {
    if let Some(word) = Word::named(name) {
        let row = word.row();
        return Some(format!(
            "{}\n{}\nArguments: {}\nExample: {}\n",
            row.name,
            row.about,
            row.effect
                .split_once("--")
                .map_or(row.effect, |(args, _)| args.trim()),
            row.example
        ));
    }
    if let Ok(docs) = crate::script::docs()
        && let Some(doc) = docs.iter().find(|doc| doc.name == name)
    {
        return Some(format!(
            "{}\nParameters: {}\n{}\n{}\n",
            doc.name, doc.parameters, doc.documentation, doc.source
        ));
    }
    guide(name)
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
mod tests {
    use super::*;
    #[test]
    fn every_word_has_its_help() -> Result<(), String> {
        for word in Word::ALL {
            let name = word.row().name;
            assert!(explain(name).is_some(), "{name}");
        }
        for doc in crate::script::docs().map_err(|e| e.to_string())? {
            assert!(explain(&doc.name).is_some(), "{}", doc.name);
        }
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
    fn the_agent_page_fits_in_100_lines() {
        let page = explain("agents").unwrap_or_default();
        assert!(!page.is_empty());
        assert!(page.lines().count() <= 100);
    }
    #[test]
    fn the_guide_lists_its_topics_as_its_headings_are() {
        for section in AGENTS.split("\n## ").skip(1) {
            let topic = section.split(':').next().unwrap_or_default();
            assert!(explain(topic).is_some(), "{topic}");
        }
    }
    #[test]
    fn the_json_guide_states_this_protocol_and_its_codes() {
        assert!(DATA.contains(&format!("protocol {}", fux_wire::protocol::PROTOCOL)));
        for code in fux_wire::code::Code::all() {
            assert!(DATA.contains(&format!("`{code}`")), "{code}");
        }
    }
}
