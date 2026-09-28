//! Command lines, parsed by the baseline and the current fux: the same
//! commands, the same usage messages, the same labels, and the same targets
//! read from single words.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

const NAMES: &[&str] = &[
    "select-pane",
    "select-pane",
    "select-tab",
    "select-tab",
    "select-workspace",
    "select-workspace",
    "reorder",
    "menu",
    "choose-tab",
    "choose-workspace",
    "choose-pane",
    "zoom",
    "copy-mode",
    "rename-prompt",
    "confirm-close",
    "command-column",
    "command-prompt",
    "split",
    "move-pane",
    "swap-pane",
    "resize-pane",
    "kill-pane",
    "kill-tab",
    "kill-workspace",
    "new-tab",
    "new-workspace",
    "rename",
    "send-keys",
    "capture-pane",
    "capture-client",
    "detach",
    "ls",
    "list",
    "paste-buffer",
    "show-buffer",
    "list-buffers",
    "terminate",
    "set",
    "bind",
    "unbind",
    "unbind-all",
    "reload",
    "list-keys",
    "kill-server",
    "bogus",
    "",
];

const WORDS: &[&str] = &[
    "--next",
    "--previous",
    "--last",
    "-L",
    "-R",
    "-U",
    "-D",
    "-t",
    "-t",
    "-t",
    "%1",
    "%2",
    "@1",
    "@3",
    "+1",
    "+2",
    "main",
    "work",
    "-c",
    "c1",
    "1",
    "--move",
    "pane",
    "tab",
    "workspace",
    "--",
    "x",
    "-h",
    "-v",
    "--to",
    "new-tab",
    "new-workspace",
    "-n",
    "--json",
    "-b",
    "0",
    "-",
    "%",
    "@",
    "+",
    "%x",
    "-S",
    "-10",
    "c",
    "界",
    "",
    "-l",
    "Enter",
    "C-c",
    "99999999999",
];

/// A command line: words from the lists, or a select-* line shaped as they
/// are written, with a slip now and then.
fn line(r: &mut Rng) -> Vec<String> {
    let word = |r: &mut Rng, items: &[&str]| r.pick(items).copied().unwrap_or_default().to_owned();
    if r.chance(40) {
        let mut argv = vec![word(r, &["select-pane", "select-tab", "select-workspace"])];
        for _ in 0..r.below(2).saturating_add(1) {
            match r.below(4) {
                0 => argv.push(word(r, &["--next", "--previous", "--last"])),
                1 => argv.push(word(r, &["-L", "-R", "-U", "-D"])),
                2 => argv.extend([
                    "-t".into(),
                    word(r, &["%1", "@2", "+3", "main", "%", "@x", "c1"]),
                ]),
                _ => argv.extend(["-c".into(), word(r, &["c1", "2", "x"])]),
            }
        }
        if r.chance(12) {
            argv.push(word(r, WORDS));
        }
        return argv;
    }
    if r.chance(5) {
        return Vec::new();
    }
    let mut argv = vec![word(r, NAMES)];
    for _ in 0..r.below(6) {
        argv.push(word(r, WORDS));
    }
    argv
}

macro_rules! stack {
    ($name:ident, $fux:ident) => {
        mod $name {
            use $fux::command;

            /// The command or usage error, the message, and the label.
            pub fn parse(argv: &[String]) -> (String, Option<String>, String) {
                let parsed = command::parse(argv);
                let message = parsed.as_ref().err().map(ToString::to_string);
                (format!("{parsed:?}"), message, command::label(argv))
            }

            /// Every reading of one word as a target.
            pub fn targets(word: &str) -> String {
                format!(
                    "{:?} {:?} {:?} {:?} {:?}",
                    command::parse_pane(word),
                    command::parse_tab(word),
                    command::parse_workspace(word),
                    command::parse_any(word),
                    command::parse_client(word)
                )
            }
        }
    };
}

stack!(base, baseline);
stack!(cur, fux);

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut commands, mut usages, mut words) = (0u64, 0u64, 0u64);
    for case in 0..times(200_000, scale) {
        let argv = line(r);
        let (a, b) = (base::parse(&argv), cur::parse(&argv));
        if a.1.is_some() {
            bump(&mut usages);
        } else {
            bump(&mut commands);
        }
        same(&format!("line {case}: {argv:?}"), a, b)?;
        for word in &argv {
            same(
                &format!("line {case}: the word {word:?}"),
                base::targets(word),
                cur::targets(word),
            )?;
            bump(&mut words);
        }
    }
    Ok(format!(
        "{commands} commands, {usages} usage errors, {words} words read as targets"
    ))
}
