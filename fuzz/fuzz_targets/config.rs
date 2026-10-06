#![no_main]
//! Input: lines. A line starting with 0xff is structured: its next byte
//! picks `set`, `bind`, `unbind`, `unbind-all` or free words (`-n` among
//! them, for bindings without the prefix), and each byte
//! after picks a word for the next slot of that command. Any other line is
//! text, split into words as a config file's are. Each line's words go to
//! `Config::apply`.
use fux::config::{Binding, Config};
use fux::keys::{Key, KeyPress};
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
    "-n",
    "Escape",
    "S-v",
    "V",
    "F5",
    "Up",
    "M-h",
    "send-prefix",
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
/// Keys for `bind` and `unbind`: letters in both cases, named keys, chords,
/// Escape, the default prefix, and words that are no key.
const KEYS: &[&str] = &[
    "a", "g", "h", "l", "m", "n", "r", "t", "w", "z", "G", "M", "N", "T", "C-a", "Left", ":", "1",
    "é", "", "S-t", "s-g", "M-S-x", "C-S-a", "Escape", "Esc", "M-Escape", "C-b", "Enter", "F5",
    "Up", "Nope",
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
    "bell",
    "titles",
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
    "'/My Shell/zsh'",
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
                match next() % 3 {
                    0 => words.push("-r"),
                    1 => words.push("-n"),
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
            if next() % 3 == 0 {
                words.push("-n");
            }
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

/// A binding as the README describes it: keys, a command, a group, and
/// whether it repeats.
#[derive(Clone, Debug, PartialEq)]
struct Model {
    keys: Vec<KeyPress>,
    command: Vec<String>,
    group: Option<String>,
    repeat: bool,
}

/// The bindings after the prefix and without it.
#[derive(Clone, Debug, Default)]
struct Models {
    prefixed: Vec<Model>,
    root: Vec<Model>,
}

fn model_of(binding: &Binding) -> Model {
    Model {
        keys: binding.keys.clone(),
        command: binding.command.clone(),
        group: binding.group.clone(),
        repeat: binding.repeat,
    }
}

/// Whether a binding is the model's, without allocating: this runs for
/// every line.
fn same(binding: &Binding, model: &Model) -> bool {
    binding.keys == model.keys
        && binding.command == model.command
        && binding.group == model.group
        && binding.repeat == model.repeat
}

/// A key by its name, case kept: `S-` on a letter is its upper case.
fn model_key(word: &str) -> Option<KeyPress> {
    let press: KeyPress = word.parse().ok()?;
    let mut shift = false;
    let mut rest = word;
    for _ in 0..3 {
        let lower = rest.get(..2).map(str::to_ascii_uppercase);
        match lower.as_deref() {
            Some("S-") if rest.len() > 2 => shift = true,
            Some("C-" | "M-") if rest.len() > 2 => {}
            _ => break,
        }
        rest = &rest[2..];
    }
    match press.key {
        Key::Char(c) if shift && c.is_ascii_lowercase() => {
            Some(KeyPress::new(Key::Char(c.to_ascii_uppercase()), press.mods))
        }
        Key::Char(_)
        | Key::Enter
        | Key::Tab
        | Key::Escape
        | Key::Backspace
        | Key::Delete
        | Key::Insert
        | Key::Arrow(_)
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => Some(press),
    }
}

/// Escape as the command column takes it, which no binding after the
/// prefix may be.
fn column_escape(key: &KeyPress) -> bool {
    key.key == Key::Escape && !key.mods.ctrl && !key.mods.alt
}

/// What `bind` would add, and whether without the prefix, if its words are
/// well formed and its command parses.
fn model_bind(mut rest: &[String], prefix: KeyPress) -> Option<(bool, Model)> {
    let (mut group, mut repeat, mut root) = (None, false, false);
    loop {
        match rest {
            [flag, after @ ..] if flag == "-n" => {
                root = true;
                rest = after;
            }
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
    // Keys: the first word, and every word after it that names a key.
    let (_, after) = rest.split_first()?;
    let count = 1 + after
        .iter()
        .take_while(|w| w.parse::<KeyPress>().is_ok())
        .count();
    let keys: Vec<KeyPress> = rest[..count]
        .iter()
        .map(|w| model_key(w))
        .collect::<Option<_>>()?;
    if root && (keys.len() > 1 || repeat || keys.first() == Some(&prefix)) {
        return None;
    }
    if !root && keys.iter().any(column_escape) {
        return None;
    }
    let command = rest[count..].to_vec();
    let parses = fux::command::parse(&command).is_ok();
    (!command.is_empty() && parses).then_some((
        root,
        Model {
            keys,
            command,
            group,
            repeat,
        },
    ))
}

/// Applies a `bind`, `unbind` or `unbind-all` to the model, returning
/// whether it was accepted; `None` for anything else.
fn model_apply(models: &mut Models, argv: &[String], prefix: KeyPress) -> Option<bool> {
    let (name, rest) = argv.split_first()?;
    match name.as_str() {
        "bind" => {
            let Some((root, new)) = model_bind(rest, prefix) else {
                return Some(false);
            };
            if root {
                match models.root.iter_mut().find(|b| b.keys == new.keys) {
                    Some(old) => *old = new,
                    None => models.root.push(new),
                }
                return Some(true);
            }
            let model = &mut models.prefixed;
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
            if let Some((flag, rest)) = rest.split_first()
                && flag == "-n"
            {
                let [word] = rest else {
                    return Some(false);
                };
                let Some(key) = model_key(word) else {
                    return Some(false);
                };
                let before = models.root.len();
                models.root.retain(|b| b.keys != [key]);
                return Some(models.root.len() < before);
            }
            let keys: Option<Vec<KeyPress>> = rest.iter().map(|w| model_key(w)).collect();
            let Some(keys) = keys.filter(|k| !k.is_empty()) else {
                return Some(false);
            };
            let model = &mut models.prefixed;
            let before = model.len();
            model.retain(|b| !b.keys.starts_with(&keys));
            Some(model.len() < before)
        }
        "unbind-all" => {
            if !rest.is_empty() {
                return Some(false);
            }
            models.prefixed.clear();
            models.root.clear();
            Some(true)
        }
        _ => None,
    }
}

/// The rules every configuration keeps.
fn check_bindings(config: &Config) {
    for b in &config.bindings {
        assert!(!b.keys.is_empty() && !b.command.is_empty(), "{b:?}");
        assert!(!b.keys.iter().any(column_escape), "Escape bound: {b:?}");
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

/// The rules every binding without the prefix keeps.
fn check_root(config: &Config) {
    for (i, b) in config.root.iter().enumerate() {
        assert_eq!(b.keys.len(), 1, "{b:?}");
        assert!(!b.repeat && !b.command.is_empty(), "{b:?}");
        assert_ne!(b.keys, [config.prefix], "the prefix bound without itself");
        assert!(
            !config.root.iter().skip(i + 1).any(|o| o.keys == b.keys),
            "two bindings of {:?}",
            b.keys
        );
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
    let mut models = Models {
        prefixed: config.bindings.iter().map(model_of).collect(),
        root: Vec::new(),
    };
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
        match model_apply(&mut models, &argv, before.prefix) {
            Some(accepted) => assert_eq!(accepted, result.is_ok(), "{argv:?}: {result:?}"),
            None => {
                assert_eq!(config.bindings, before.bindings, "{argv:?}");
                assert_eq!(config.root, before.root, "{argv:?}");
            }
        }
        let model = &models.prefixed;
        let agree = config.bindings.len() == model.len()
            && config.bindings.iter().zip(model).all(|(b, m)| same(b, m));
        assert!(
            agree,
            "{argv:?}: {:?} but the model has {model:?}",
            config.bindings
        );
        let root = &models.root;
        let agree = config.root.len() == root.len()
            && config.root.iter().zip(root).all(|(b, m)| same(b, m));
        assert!(
            agree,
            "{argv:?}: {:?} but the model has {root:?}",
            config.root
        );
        let bindings = model;
        // What each accepted command promises.
        match (argv.first().map(String::as_str), &result) {
            (Some("bind"), Ok(())) => {
                if let Some((root, new)) = argv.get(1..).and_then(|w| model_bind(w, before.prefix))
                {
                    let list = if root { &models.root } else { bindings };
                    let same: Vec<_> = list.iter().filter(|b| b.keys == new.keys).collect();
                    assert_eq!(same, [&new], "{argv:?}");
                }
            }
            (Some("set"), Ok(())) if argv.get(1).is_some_and(|o| o == "prefix") => {
                assert!(
                    !config.root.iter().any(|b| b.keys == [config.prefix]),
                    "{argv:?}: the prefix is bound without itself"
                );
            }
            (Some("unbind"), outcome) if argv.get(1).is_none_or(|w| w != "-n") => {
                let keys: Option<Vec<KeyPress>> =
                    argv.iter().skip(1).map(|w| model_key(w)).collect();
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
            check_root(&config);
            check_describe(&config);
        }
    }
});
