//! Help from the same native operation table that executes commands.
use crate::script::Word;
pub const AGENTS: &str = include_str!("../docs/agents.md");
pub const COOKBOOK: &str = include_str!("../docs/cookbook.md");
pub const JSON: &str = include_str!("../docs/json.md");
pub fn overview() -> String {
    let mut text = String::from(
        "fux: a terminal multiplexer configured and driven in grift\n\nusage: fux '(operation arguments...)' STRING...\n       fux --file FILE STRING...\n       fux -\n       fux                 attach this terminal\n\nA command line is one quoted grift program. Following shell arguments are strings\nfrom (args). Records print as grift data; ask for fields with (get record \"field\").\nExample: fux '(split (axis (pane 3) \"horizontal\"))'\nExample: fux '(line (pane 3) \"ls -l\")'\n\nOperations:\n",
    );
    for word in Word::ALL {
        let row = word.row();
        if row.name != "json" {
            text += &format!("  {:<22} {}\n", row.name, row.about);
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
            row.name, row.about, row.effect
        ));
    }
    match name {
        "agents" | "guide" => Some(AGENTS.to_owned()),
        "cookbook" => Some(COOKBOOK.to_owned()),
        _ => None,
    }
}
