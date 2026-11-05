//! Help from the same native operation table that executes commands.
use crate::script::Word;
pub const AGENTS: &str = include_str!("../docs/agents.md");
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
    text
}
pub fn unknown(name: &str) -> String {
    format!("no operation or topic {name:?}")
}
pub fn explain(name: &str) -> Option<String> {
    if let Some(word) = Word::named(name) {
        let row = word.row();
        return Some(format!(
            "{}\n{}\nArguments: {}\n",
            row.name,
            row.about,
            row.effect
                .split_once("--")
                .map_or(row.effect, |(args, _)| args.trim())
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
    match name {
        "agents" | "guide" => Some(AGENTS.to_owned()),
        "cookbook" => Some(COOKBOOK.to_owned()),
        _ => None,
    }
}
