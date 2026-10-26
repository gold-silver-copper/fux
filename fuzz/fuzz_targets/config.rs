#![no_main]
//! Input: lines, against one client's keymap, from the defaults.
//! - A line starting with 0xff is a config statement: its next byte picks
//!   `bind`, `bind-root`, `unbind`, `unbind-root`, `unbind-all`, `prefix`,
//!   `label`, `group` or `repeat`, and each byte after picks a word for its
//!   next slot. It is written as fuxth, compiled and run as a config is.
//! - A line starting with 0xfe is keys typed at the client: its next byte
//!   says whether the client only watches (its top bit), and each byte
//!   after picks a key for `Held::press`.
//! - Any other line is fuxth source: split into tokens, compiled, and run
//!   as a config for a bounded number of steps, up to its first request
//!   for the server.
use fux::keymap::{Binding, Held, Keymap, Layer, Node};
use fux::keys::{KeyPress, Keystroke};
use fux::screens::Effect;
use fux::script::{Bound, Fux, Host};
use fuxth::{Machine, Outcome};
use libfuzzer_sys::fuzz_target;

/// Keys for bindings, one or several: letters in both cases, named keys,
/// chords, Escape, the default prefix, and words that are no key.
const KEYS: &[&str] = &[
    "a", "g", "h", "l", "m", "n", "r", "t", "w", "z", "G", "M", "N", "T", "C-a", "Left", ":", "1",
    "é", "", "S-t", "s-g", "M-S-x", "C-S-a", "Escape", "Esc", "M-Escape", "C-b", "Enter", "F5",
    "Up", "Nope", "g n", "t m h", "r", "t n", "y l", "y h", "  ", "a Escape", "g Nope",
];
/// Code to bind: programs that compile, and some that do not.
const CODES: &[&str] = &[
    "me zoom",
    "opts new-tab drop",
    "me focused str right 1 move-border",
    "me str next focus",
    "",
    "# only a comment",
    "'unclosed",
    "str 'a b' drop",
    "nope-no-word",
    "define x me zoom end x",
    "1 if me zoom then",
    "if",
];
/// Labels and groups.
const TEXTS: &[&str] = &[
    "Tools", "My group", "", "Other", "Reorder", "Panes", "'", "é",
];
/// Keys typed: the prefix, the defaults' letters, Escape, Enter, the
/// arrows and paging keys, and keys some lines bind.
const PRESSES: &[&str] = &[
    "C-b", "C-a", "Escape", "Enter", "Up", "Down", "PageUp", "PageDown", "Home", "End", "t", "w",
    "r", "m", "h", "l", "n", "g", "y", "z", "x", "d", "e", "G", "M-t", "F5", "Left", "1", "é", "a",
];
/// How many steps a free program may take: enough for any config, few
/// enough for a loop to end.
const BUDGET: u64 = 20_000;

fn pick<'a>(list: &[&'a str], b: u8) -> &'a str {
    list[usize::from(b) % list.len()]
}

/// A word as fuxth reads it back: bare if nothing in it is special, else
/// in single quotes. As dumps quote (`session::layouts::quote`).
fn quote(word: &str) -> String {
    let bare = !word.is_empty()
        && !word.starts_with('#')
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    match bare {
        true => word.to_owned(),
        false => format!("'{}'", word.replace('\'', "'\\''")),
    }
}

/// A config statement, picked: the word, and the strings it takes.
#[derive(Clone, Debug)]
enum Statement {
    Bind {
        keys: String,
        code: String,
        root: bool,
    },
    Unbind {
        keys: String,
        root: bool,
    },
    UnbindAll,
    Prefix(String),
    Label(String),
    Group(String),
    Repeat,
}

impl Statement {
    fn picked(kind: u8, picks: &[u8]) -> Statement {
        let mut picks = picks.iter().copied();
        let mut next = || picks.next().unwrap_or(0);
        match kind % 9 {
            0 | 1 => Statement::Bind {
                keys: pick(KEYS, next()).to_owned(),
                code: pick(CODES, next()).to_owned(),
                root: kind % 9 == 1,
            },
            2 | 3 => Statement::Unbind {
                keys: pick(KEYS, next()).to_owned(),
                root: kind % 9 == 3,
            },
            4 => Statement::UnbindAll,
            5 => Statement::Prefix(pick(KEYS, next()).to_owned()),
            6 => Statement::Label(pick(TEXTS, next()).to_owned()),
            7 => Statement::Group(pick(TEXTS, next()).to_owned()),
            _ => Statement::Repeat,
        }
    }

    /// The statement as a config file writes it.
    fn fuxth(&self) -> String {
        let str = |s: &str| format!("str {}", quote(s));
        match self {
            Statement::Bind { keys, code, root } => {
                let word = if *root { "bind-root" } else { "bind" };
                format!("{} {} {word}", str(keys), str(code))
            }
            Statement::Unbind { keys, root } => {
                let word = if *root { "unbind-root" } else { "unbind" };
                format!("{} {word}", str(keys))
            }
            Statement::UnbindAll => "unbind-all".to_owned(),
            Statement::Prefix(key) => format!("{} prefix", str(key)),
            Statement::Label(text) => format!("{} label", str(text)),
            Statement::Group(text) => format!("{} group", str(text)),
            Statement::Repeat => "repeat".to_owned(),
        }
    }
}

/// A binding after the prefix as the README describes it.
#[derive(Clone, Debug, PartialEq)]
struct Model {
    keys: Vec<KeyPress>,
    binding: Binding,
    repeat: bool,
}

/// The client's keys as a model: its prefix, its bindings after the prefix
/// and without it, and the binding `label`, `group` and `repeat` change.
#[derive(Clone, Debug)]
struct Models {
    prefix: KeyPress,
    prefixed: Vec<Model>,
    root: Vec<(KeyPress, Binding)>,
    last: Option<Bound>,
}

impl Models {
    /// What `keymap` holds, as a model.
    fn of(keymap: &Keymap, last: Option<Bound>) -> Models {
        Models {
            prefix: keymap.prefix,
            prefixed: models_of(&keymap.bindings),
            root: keymap.root.clone(),
            last,
        }
    }

    /// Applies `statement`: whether it was accepted.
    fn apply(&mut self, statement: &Statement) -> bool {
        let escape = |k: &KeyPress| k.key == fux::keys::Key::Escape && !k.mods.ctrl && !k.mods.alt;
        match statement {
            Statement::Bind { keys, code, root } => {
                // The code is compiled as it is bound.
                if !compiles(code) {
                    return false;
                }
                let Ok(keys) = fux::keymap::keys(keys) else {
                    return false;
                };
                if *root {
                    let ([key], false) = (keys.as_slice(), keys.first() == Some(&self.prefix))
                    else {
                        return false;
                    };
                    let binding = Binding {
                        code: code.clone(),
                        label: code.clone(),
                        group: "Without the prefix".to_owned(),
                    };
                    match self.root.iter_mut().find(|(k, _)| k == key) {
                        Some((_, old)) => *old = binding,
                        None => self.root.push((*key, binding)),
                    }
                    self.last = Some(Bound::Root(*key));
                    return true;
                }
                // A sequence is a binding or a layer, never both; and
                // Escape after the prefix closes the column.
                let conflict = self.prefixed.iter().any(|b| {
                    b.keys.len() != keys.len()
                        && (b.keys.starts_with(&keys) || keys.starts_with(&b.keys))
                });
                if conflict || keys.iter().any(escape) {
                    return false;
                }
                let new = Model {
                    keys: keys.clone(),
                    binding: Binding {
                        code: code.clone(),
                        label: code.clone(),
                        group: "Other".to_owned(),
                    },
                    repeat: false,
                };
                match self.prefixed.iter_mut().find(|b| b.keys == keys) {
                    Some(old) => *old = new,
                    None => self.prefixed.push(new),
                }
                self.last = Some(Bound::Prefix(keys));
                true
            }
            Statement::Unbind { keys, root: true } => {
                let Ok(key) = fux::keymap::key(keys) else {
                    return false;
                };
                let before = self.root.len();
                self.root.retain(|(k, _)| *k != key);
                self.root.len() < before
            }
            Statement::Unbind { keys, root: false } => {
                let Ok(keys) = fux::keymap::keys(keys) else {
                    return false;
                };
                let before = self.prefixed.len();
                self.prefixed.retain(|b| !b.keys.starts_with(&keys));
                self.prefixed.len() < before
            }
            Statement::UnbindAll => {
                self.prefixed.clear();
                self.root.clear();
                true
            }
            Statement::Prefix(key) => {
                let Ok(key) = fux::keymap::key(key) else {
                    return false;
                };
                if self.root.iter().any(|(k, _)| *k == key) {
                    return false;
                }
                self.prefix = key;
                true
            }
            Statement::Label(text) | Statement::Group(text) => {
                let label = matches!(statement, Statement::Label(_));
                let binding = match &self.last {
                    None => None,
                    Some(Bound::Root(key)) => (self.root.iter_mut())
                        .find(|(k, _)| k == key)
                        .map(|(_, b)| b),
                    Some(Bound::Prefix(keys)) => (self.prefixed.iter_mut())
                        .find(|b| b.keys == *keys)
                        .map(|b| &mut b.binding),
                    // The fuzz writes no screen's keys.
                    Some(Bound::In(..)) => None,
                };
                let Some(binding) = binding else {
                    return false;
                };
                match label {
                    true => binding.label = text.clone(),
                    false => binding.group = text.clone(),
                }
                true
            }
            Statement::Repeat => {
                let Some(Bound::Prefix(keys)) = &self.last else {
                    return false;
                };
                match self.prefixed.iter_mut().find(|b| b.keys == *keys) {
                    Some(b) => {
                        b.repeat = true;
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// Whether `keymap` holds what the model does: the bindings after the
    /// prefix in any order (a layer lists its keys together), those
    /// without it in the order bound.
    fn agree(&self, keymap: &Keymap) -> bool {
        let all = models_of(&keymap.bindings);
        keymap.prefix == self.prefix
            && keymap.root == self.root
            && all.len() == self.prefixed.len()
            && self.prefixed.iter().all(|m| all.contains(m))
    }
}

/// The bindings after the prefix as models, depth first.
fn models_of(layer: &Layer) -> Vec<Model> {
    (layer.all().into_iter())
        .map(|(keys, binding, repeat)| Model {
            keys,
            binding: binding.clone(),
            repeat,
        })
        .collect()
}

fn compiles(code: &str) -> bool {
    fuxth::token::source("binding", code)
        .ok()
        .is_some_and(|tokens| fux::script::compile(&tokens).is_ok())
}

/// The rules every keymap keeps that its types do not: no empty layer, no
/// Escape after the prefix, no binding of the prefix without it nor of one
/// key twice; and every layer's column shows.
fn check_keymap(keymap: &Keymap) {
    fn check_layer(keymap: &Keymap, path: &mut Vec<KeyPress>, layer: &Layer) {
        let column = keymap.column(path, usize::MAX);
        let column = column.unwrap_or_else(|| panic!("no column for {path:?}"));
        assert!(column.selected < column.entries.len().max(1));
        for (key, node) in layer.iter() {
            if let Node::Layer(inner) = node {
                assert!(inner.first().is_some(), "an empty layer at {path:?} {key}");
                path.push(*key);
                check_layer(keymap, path, inner);
                path.pop();
            }
        }
    }
    check_layer(keymap, &mut Vec::new(), &keymap.bindings);
    for (keys, _, _) in keymap.bindings.all() {
        let escape = |k: &KeyPress| k.key == fux::keys::Key::Escape && !k.mods.ctrl && !k.mods.alt;
        assert!(!keys.iter().any(escape), "Escape bound: {keys:?}");
    }
    for (i, (key, _)) in keymap.root.iter().enumerate() {
        assert_ne!(*key, keymap.prefix, "the prefix bound without itself");
        let again = keymap.root.iter().skip(i + 1).any(|(o, _)| o == key);
        assert!(!again, "two bindings of {key:?}");
    }
}

/// The defaults, built once.
fn defaults() -> &'static Keymap {
    static DEFAULTS: std::sync::OnceLock<Keymap> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| fux::script::default_keys().expect("the default keys bind"))
}

/// Runs `machine` as a config against `keymap`, for at most `BUDGET`
/// steps, until it ends, fails, or asks the server something (there is
/// none). Whether it ran to its end, and the binding it made last.
fn run_config(
    machine: &mut Machine<Fux>,
    keymap: &mut Keymap,
    last: Option<Bound>,
) -> (bool, Option<Bound>) {
    let mut out = Vec::new();
    let mut host = Host {
        context: fux::protocol::Context::default(),
        keymap: Some(keymap),
        last_bound: last,
        out: &mut out,
    };
    let finished = matches!(machine.run(&mut host, BUDGET), Ok(Outcome::Finished));
    (finished, host.last_bound)
}

/// The keys of the client, as its own: the keymap, what its keys are in
/// the middle of, and whether the keymap changed under a column or repeat
/// mode since it opened (the client then finds out at its next key).
struct Client {
    keymap: Keymap,
    held: Held,
    stale: bool,
}

impl Client {
    /// A key typed, by a client that types or only watches.
    fn press(&mut self, read_only: bool, press: KeyPress) {
        let held_before = self.held.clone();
        let stroke = Keystroke::from(press);
        let steps = self.held.press(&self.keymap, stroke, read_only);
        let codes: Vec<&str> = (self.keymap.bindings.all().into_iter())
            .map(|(_, b, _)| b.code.as_str())
            .chain(self.keymap.root.iter().map(|(_, b)| b.code.as_str()))
            .collect();
        for step in &steps {
            if let Effect::Run(fux::screens::Job { code, .. }) = step {
                // Only a bound program runs, and it compiles: it did when it
                // was bound.
                assert!(
                    codes.contains(&code.as_str()) || (read_only && code == "me detach"),
                    "{code:?} runs but is not bound"
                );
                assert!(compiles(code), "{code:?} runs but does not compile");
            }
        }
        if !read_only && held_before == Held::Normal {
            let root = self.keymap.root.iter().find(|(k, _)| *k == press);
            let expected = if press == self.keymap.prefix {
                let column = self.keymap.column(&[], 0);
                vec![Effect::Send(fux::act::Act::Show(fux::act::Panel::Column(
                    column.unwrap_or_default(),
                )))]
            } else if let Some((_, binding)) = root {
                vec![Effect::Run(binding.code.clone().into())]
            } else {
                vec![Effect::Send(fux::act::Act::Key(stroke))]
            };
            assert_eq!(steps, expected, "{press} typed");
        }
        if matches!(self.held, Held::Normal | Held::Prefixed) {
            self.stale = false;
        }
        if self.stale {
            return;
        }
        match &self.held {
            Held::Column { path, selected } => {
                let column = self.keymap.column(path, *selected);
                let column = column.unwrap_or_else(|| panic!("a column for {path:?}, gone"));
                assert_eq!(column.selected, *selected);
            }
            Held::Repeat { path } => assert!(
                self.keymap.repeat_bar(path).is_some(),
                "repeating {path:?}, which holds no repeating binding"
            ),
            Held::Normal | Held::Prefixed => {}
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut client = Client {
        keymap: defaults().clone(),
        held: Held::Normal,
        stale: false,
    };
    let mut models = Models::of(&client.keymap, None);
    check_keymap(&client.keymap);
    for line in data.split(|&b| b == b'\n') {
        let before = client.keymap.clone();
        match line.split_first() {
            Some((&0xff, rest)) => {
                let Some((&kind, picks)) = rest.split_first() else {
                    continue;
                };
                let statement = Statement::picked(kind, picks);
                let text = statement.fuxth();
                let tokens = fuxth::token::source("config", &text);
                let tokens = tokens.unwrap_or_else(|e| panic!("{text:?}: {e}"));
                let compiled = fux::script::compile(&tokens);
                let mut machine = compiled.unwrap_or_else(|e| panic!("{text:?}: {e}"));
                let last = models.last.clone();
                let (accepted, last) = run_config(&mut machine, &mut client.keymap, last);
                if !accepted {
                    assert_eq!(
                        client.keymap, before,
                        "{text:?} failed but changed the keys"
                    );
                }
                let expected = models.apply(&statement);
                assert_eq!(accepted, expected, "{text:?}");
                assert_eq!(last, models.last, "{text:?}: the binding made last");
                assert!(
                    models.agree(&client.keymap),
                    "{text:?}: {:?} but the model has {models:?}",
                    client.keymap
                );
            }
            Some((&0xfe, rest)) => {
                let Some((&how, keys)) = rest.split_first() else {
                    continue;
                };
                let read_only = how & 0x80 != 0;
                for &k in keys {
                    let Ok(press) = fux::keymap::key(pick(PRESSES, k)) else {
                        continue;
                    };
                    client.press(read_only, press);
                }
            }
            _ => {
                let text = String::from_utf8_lossy(line);
                let Ok(tokens) = fuxth::token::source("fuzz", &text) else {
                    continue;
                };
                // Written as a dump writes words, they read back.
                let texts: Vec<&str> = tokens.iter().map(|t| &*t.text).collect();
                let quoted: Vec<String> = texts.iter().map(|t| quote(t)).collect();
                let again = fuxth::token::source("again", &quoted.join(" "));
                let again = again.unwrap_or_else(|e| panic!("{quoted:?}: {e}"));
                let again: Vec<&str> = again.iter().map(|t| &*t.text).collect();
                assert_eq!(again, texts, "{text:?}");
                let mut machine = match fux::script::compile(&tokens) {
                    Ok(machine) => machine,
                    Err(failed) => {
                        assert_eq!(failed.code.status().get(), 1, "{text:?}: {failed}");
                        assert!(!failed.code.name().is_empty(), "{text:?}: {failed}");
                        continue;
                    }
                };
                // Words that read files: a fuzzer finds /dev/zero.
                if texts.contains(&"check") {
                    continue;
                }
                let last = models.last.clone();
                let (_, last) = run_config(&mut machine, &mut client.keymap, last);
                models = Models::of(&client.keymap, last);
            }
        }
        if client.keymap != before {
            check_keymap(&client.keymap);
            client.stale |= client.held != Held::Normal;
        }
    }
});
