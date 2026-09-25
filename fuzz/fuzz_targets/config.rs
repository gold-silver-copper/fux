#![no_main]
//! Input: lines. A line starting with 0xff is structured: its next byte
//! picks `set`, `bind`, `unbind`, `unbind-all` or free words, and each byte
//! after picks a word for the next slot of that command. Any other line is
//! text, split into words as a config file's are. Each line's words go to
//! `Config::apply`.
use fux::config::{Binding, Config};
use fux::keys::{Key, Modifiers};
use fux::words;
use libfuzzer_sys::fuzz_target;

/// Words for structured lines: the commands, flags, keys both valid and not,
/// groups, commands to bind, and options with values.
const WORDS: &[&str] = &[
    "bind",
    "unbind",
    "unbind-all",
    "set",
    "-g",
    "-r",
    "a",
    "b",
    "g",
    "h",
    "l",
    "m",
    "n",
    "r",
    "t",
    "w",
    "z",
    "A",
    "G",
    "M",
    "N",
    "T",
    "C-a",
    "Left",
    "Tab",
    ":",
    "1",
    "é",
    "#",
    "'",
    "",
    "Tools",
    "My group",
    "zoom",
    "split",
    "-h",
    "new-tab",
    "--",
    "htop",
    "prefix",
    "shell",
    "history-lines",
    "clipboard",
    "buffers",
    "C-b",
    "M-x",
    "/bin/sh",
    "-l",
    "on",
    "off",
    "0",
    "16",
    "1000001",
];
/// Keys for `bind` and `unbind`: letters in both cases, and some that are
/// not letters.
const KEYS: &[&str] = &[
    "a", "g", "h", "l", "m", "n", "r", "t", "w", "z", "G", "M", "N", "T", "C-a", "Left", ":", "1",
    "é", "",
];
const GROUPS: &[&str] = &["Tools", "My group", "", "-r", "Reorder"];
const COMMANDS: &[&[&str]] = &[
    &["zoom"],
    &["split", "-h"],
    &["new-tab"],
    &["select-tab", "--next"],
    &["-r", "zoom"],
    &["split", "-v", "--", "htop", "a b"],
];
const OPTIONS: &[&str] = &[
    "prefix",
    "shell",
    "history-lines",
    "clipboard",
    "buffers",
    "nope",
];
/// Values for `set`: valid and not, and words that only survive a round
/// trip through `describe()` if it quotes them.
const VALUES: &[&str] = &[
    "C-a",
    "M-x",
    "Space",
    "BTab",
    "F5",
    "a",
    "#",
    "'",
    "\\",
    "\"",
    " ",
    "\u{3000}",
    "é",
    "/bin/sh",
    "/My Shell/zsh",
    "/bin/zsh -l",
    "-l",
    "",
    "on",
    "off",
    "write-only",
    "0",
    "16",
    "1000",
    "1000001",
    "+5",
];

fn pick<'a>(list: &[&'a str], b: u8) -> &'a str {
    list[usize::from(b) % list.len()]
}

/// A structured line: `kind` picks the command, `picks` its words.
fn structured(kind: u8, picks: &[u8]) -> Vec<String> {
    let mut picks = picks.iter().copied();
    let mut next = || picks.next().unwrap_or(0);
    let mut words: Vec<&str> = Vec::new();
    match kind % 5 {
        0 => {
            words.extend(["set", pick(OPTIONS, next())]);
            for _ in 0..1 + next() % 2 {
                words.push(pick(VALUES, next()));
            }
        }
        1 => {
            words.push("bind");
            for _ in 0..next() % 3 {
                match next() % 2 {
                    0 => words.push("-r"),
                    _ => words.extend(["-g", pick(GROUPS, next())]),
                }
            }
            for _ in 0..1 + next() % 3 {
                words.push(pick(KEYS, next()));
            }
            words.extend(COMMANDS[usize::from(next()) % COMMANDS.len()]);
        }
        2 => {
            words.push("unbind");
            for _ in 0..next() % 4 {
                words.push(pick(KEYS, next()));
            }
        }
        3 => {
            words.push("unbind-all");
            if next() % 4 == 0 {
                words.push("a");
            }
        }
        _ => {
            for b in picks.by_ref() {
                words.push(pick(WORDS, b));
            }
        }
    }
    words.into_iter().map(str::to_owned).collect()
}

/// A binding as the README describes it: letters, a command, a group, and
/// whether it repeats.
#[derive(Clone, Debug, PartialEq)]
struct Model {
    keys: Vec<char>,
    command: Vec<String>,
    group: Option<String>,
    repeat: bool,
}

fn model_of(binding: &Binding) -> Model {
    Model {
        keys: binding
            .keys
            .iter()
            .map(|k| match k.key {
                Key::Char(c) if k.mods == Modifiers::NONE => c,
                _ => '\0',
            })
            .collect(),
        command: binding.command.clone(),
        group: binding.group.clone(),
        repeat: binding.repeat,
    }
}

/// Whether a binding is the model's, without allocating: this runs for
/// every line.
fn same(binding: &Binding, model: &Model) -> bool {
    binding.keys.len() == model.keys.len()
        && binding
            .keys
            .iter()
            .zip(&model.keys)
            .all(|(k, &c)| k.key == Key::Char(c) && k.mods == Modifiers::NONE)
        && binding.command == model.command
        && binding.group == model.group
        && binding.repeat == model.repeat
}

/// A key after the prefix: one ASCII letter, in lower case.
fn model_letter(word: &str) -> Option<char> {
    let mut chars = word.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// What `bind` would add, if its words are well formed.
fn model_bind(mut rest: &[String]) -> Option<Model> {
    let (mut group, mut repeat) = (None, false);
    loop {
        match rest {
            [flag, name, after @ ..] if flag == "-g" => {
                group = Some(name.clone());
                rest = after;
            }
            [flag] if flag == "-g" => return None,
            [flag, after @ ..] if flag == "-r" => {
                repeat = true;
                rest = after;
            }
            _ => break,
        }
    }
    // Keys: the first word, and every one-character word after it.
    let (_, after) = rest.split_first()?;
    let count = 1 + after.iter().take_while(|w| w.chars().count() == 1).count();
    let keys: Vec<char> = rest[..count]
        .iter()
        .map(|w| model_letter(w))
        .collect::<Option<_>>()?;
    let command = rest[count..].to_vec();
    (!command.is_empty()).then_some(Model {
        keys,
        command,
        group,
        repeat,
    })
}

/// Applies a `bind`, `unbind` or `unbind-all` to the model, returning
/// whether it was accepted; `None` for anything else.
fn model_apply(model: &mut Vec<Model>, argv: &[String]) -> Option<bool> {
    let (name, rest) = argv.split_first()?;
    match name.as_str() {
        "bind" => {
            let Some(new) = model_bind(rest) else {
                return Some(false);
            };
            // A sequence is a command or a layer, never both.
            let conflict = model.iter().any(|b| {
                b.keys.len() != new.keys.len()
                    && (b.keys.starts_with(&new.keys) || new.keys.starts_with(&b.keys))
            });
            if conflict {
                return Some(false);
            }
            match model.iter_mut().find(|b| b.keys == new.keys) {
                Some(old) => *old = new,
                None => model.push(new),
            }
            Some(true)
        }
        "unbind" => {
            let keys: Option<Vec<char>> = rest.iter().map(|w| model_letter(w)).collect();
            let Some(keys) = keys.filter(|k| !k.is_empty()) else {
                return Some(false);
            };
            let before = model.len();
            model.retain(|b| !b.keys.starts_with(&keys));
            Some(model.len() < before)
        }
        "unbind-all" => {
            if !rest.is_empty() {
                return Some(false);
            }
            model.clear();
            Some(true)
        }
        _ => None,
    }
}

/// The rules every configuration keeps.
fn check_bindings(config: &Config) {
    for b in &config.bindings {
        assert!(!b.keys.is_empty() && !b.command.is_empty(), "{b:?}");
        for key in &b.keys {
            assert!(
                matches!(key.key, Key::Char('a'..='z')) && key.mods == Modifiers::NONE,
                "{b:?}"
            );
        }
        for other in &config.bindings {
            if std::ptr::eq(b, other) {
                continue;
            }
            assert_ne!(b.keys, other.keys, "two bindings of the same keys");
            assert!(
                !other.keys.starts_with(&b.keys),
                "{:?} is both a command and a layer of {:?}",
                b.keys,
                other.keys
            );
        }
    }
}

/// The defaults, built once: each is a few dozen `bind`s.
fn defaults() -> &'static Config {
    static DEFAULTS: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(Config::default)
}

/// `describe()` read back gives the same configuration.
fn check_describe(config: &Config) {
    let mut fresh = defaults().clone();
    assert!(fresh.apply(&["unbind-all".to_owned()]).is_ok());
    for line in config.describe() {
        let argv = words::split(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        if let Err(e) = fresh.apply(&argv) {
            panic!("{line:?}: {e}");
        }
    }
    // The lines first: they say what differs far more briefly.
    assert_eq!(fresh.describe(), config.describe());
    assert!(&fresh == config, "the same lines, but not the same config");
}

fn check_words(words: &[String]) {
    let joined = words::join(words);
    assert_eq!(words::split(&joined).as_deref(), Ok(words), "{joined:?}");
}

fn line_words(line: &[u8]) -> Option<Vec<String>> {
    match line.split_first() {
        Some((0xff, rest)) => {
            let (&kind, picks) = rest.split_first()?;
            Some(structured(kind, picks))
        }
        _ => {
            let text = String::from_utf8_lossy(line);
            let words = words::split(&text).ok()?;
            check_words(&words);
            Some(words)
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut config = defaults().clone();
    let mut model: Vec<Model> = config.bindings.iter().map(model_of).collect();
    check_bindings(&config);
    check_describe(&config);
    for line in data.split(|&b| b == b'\n') {
        let Some(argv) = line_words(line) else {
            continue;
        };
        if argv.is_empty() {
            continue;
        }
        check_words(&argv);
        let before = config.clone();
        let result = config.apply(&argv);
        if result.is_err() {
            assert_eq!(config, before, "{argv:?} failed but changed the config");
        }
        match model_apply(&mut model, &argv) {
            Some(accepted) => assert_eq!(accepted, result.is_ok(), "{argv:?}: {result:?}"),
            None => assert_eq!(config.bindings, before.bindings, "{argv:?}"),
        }
        let agree = config.bindings.len() == model.len()
            && config.bindings.iter().zip(&model).all(|(b, m)| same(b, m));
        assert!(
            agree,
            "{argv:?}: {:?} but the model has {model:?}",
            config.bindings
        );
        let bindings = &model;
        // What each accepted command promises.
        match (argv.first().map(String::as_str), &result) {
            (Some("bind"), Ok(())) => {
                if let Some(new) = argv.get(1..).and_then(model_bind) {
                    let same: Vec<_> = bindings.iter().filter(|b| b.keys == new.keys).collect();
                    assert_eq!(same, [&new], "{argv:?}");
                }
            }
            (Some("unbind"), outcome) => {
                let keys: Option<Vec<char>> =
                    argv.iter().skip(1).map(|w| model_letter(w)).collect();
                if let Some(keys) = keys.filter(|k| !k.is_empty()) {
                    // Nothing starts with them now; if it failed, nothing did.
                    assert!(!bindings.iter().any(|b| b.keys.starts_with(&keys)));
                    if outcome.is_err() {
                        assert!(
                            !before
                                .bindings
                                .iter()
                                .map(model_of)
                                .any(|b| b.keys.starts_with(&keys))
                        );
                    }
                }
            }
            _ => {}
        }
        // An unchanged configuration was checked already.
        if config != before {
            check_bindings(&config);
            check_describe(&config);
        }
    }
});
