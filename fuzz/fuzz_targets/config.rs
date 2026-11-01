#![no_main]
//! Input: lines, against one client's keymap, from the defaults.
//! - A line starting with 0xff is a config statement: its next byte picks
//!   `bind`, `unbind`, `unbind-all`, `move-keys`, `label`, `group` or
//!   `repeat`, and each byte after picks a word for its next slot. It is
//!   written as fuxth, and run as a config is.
//! - A line starting with 0xfe is keys typed at the client: its next byte
//!   says whether the client only watches (its top bit), and each byte
//!   after picks a key for `Held::press`.
//! - Any other line is fuxth source: split into tokens, and run
//!   as a config for a bounded number of steps, up to its first request
//!   for the server.
use fux::keymap::{Binding, Entry, Held, Keymap, Map};
use fux::screens::Effect;
use fux::script::{Bound, Fux, Host};
use fux_vt::keys::{KeyPress, Keystroke};
use fuxth::{Machine, Outcome};
use libfuzzer_sys::fuzz_target;

/// Keys for bindings, one or several from the root: letters in both
/// cases, named keys, chords, Escape, the default prefix and keys after
/// it, and words that are no key.
const KEYS: &[&str] = &[
    "a",
    "g",
    "h",
    "l",
    "m",
    "n",
    "r",
    "t",
    "w",
    "z",
    "G",
    "M",
    "N",
    "T",
    "C-a",
    "Left",
    ":",
    "1",
    "é",
    "",
    "S-t",
    "s-g",
    "M-S-x",
    "C-S-a",
    "Escape",
    "Esc",
    "M-Escape",
    "C-b",
    "Enter",
    "F5",
    "Up",
    "Nope",
    "g n",
    "C-b g n",
    "C-b t m h",
    "C-b t m",
    "C-b r",
    "C-b t n",
    "C-b y l",
    "C-b y h",
    "C-b y",
    "  ",
    "C-b a Escape",
    "C-b g Nope",
    "C-a g",
    "C-b C-b",
    "C-b t",
];
/// Code to bind, written between `code` and `end`, and whether it
/// compiles there: words the config's dictionary has, and some it does
/// not, or that close or comment out the `end`.
const CODES: &[(&str, bool)] = &[
    ("me focused zoom", true),
    ("opts new-tab drop", true),
    ("me focused str right 1 move-border", true),
    ("me str next focus", true),
    ("", true),
    ("str 'a b' drop", true),
    ("1 if me focused zoom then", true),
    ("# only a comment", false),
    ("'unclosed", false),
    ("nope-no-word", false),
    ("define x me focused zoom end x", false),
    ("if", false),
    ("end", false),
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
/// in single quotes, as native hints and list-keys quote words.
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
    Bind { keys: String, code: String },
    Unbind(String),
    UnbindAll,
    MoveKeys { from: String, to: String },
    Label(String),
    Group(String),
    Repeat(String),
}

impl Statement {
    fn picked(kind: u8, picks: &[u8]) -> Statement {
        let mut picks = picks.iter().copied();
        let mut next = || picks.next().unwrap_or(0);
        match kind % 8 {
            0 | 1 => Statement::Bind {
                keys: pick(KEYS, next()).to_owned(),
                code: CODES[usize::from(next()) % CODES.len()].0.to_owned(),
            },
            2 => Statement::Unbind(pick(KEYS, next()).to_owned()),
            3 => Statement::UnbindAll,
            4 => Statement::MoveKeys {
                from: pick(KEYS, next()).to_owned(),
                to: pick(KEYS, next()).to_owned(),
            },
            5 => Statement::Label(pick(TEXTS, next()).to_owned()),
            6 => Statement::Group(pick(TEXTS, next()).to_owned()),
            _ => Statement::Repeat(pick(KEYS, next()).to_owned()),
        }
    }

    /// The statement as a config file writes it.
    fn fuxth(&self) -> String {
        let str = |s: &str| format!("str {}", quote(s));
        match self {
            Statement::Bind { keys, code } => format!("{} code {code} end bind", str(keys)),
            Statement::Unbind(keys) => format!("{} unbind", str(keys)),
            Statement::UnbindAll => "unbind-all".to_owned(),
            Statement::MoveKeys { from, to } => format!("{} {} move-keys", str(from), str(to)),
            Statement::Label(text) => format!("{} label", str(text)),
            Statement::Group(text) => format!("{} group", str(text)),
            Statement::Repeat(keys) => format!("{} repeat", str(keys)),
        }
    }
}

/// The client's keys as the README describes them, flat: each binding by
/// its keys from the root, the maps that repeat by theirs, and the binding
/// `label` and `group` change.
#[derive(Clone, Debug)]
struct Models {
    bound: Vec<(Vec<KeyPress>, Binding)>,
    repeats: Vec<Vec<KeyPress>>,
    last: Option<Bound>,
}

/// Whether one of two key sequences starts the other, but they differ: a
/// key would be both a binding and a map.
fn overlap(a: &[KeyPress], b: &[KeyPress]) -> bool {
    a.len() != b.len() && (a.starts_with(b) || b.starts_with(a))
}

fn escape(k: &KeyPress) -> bool {
    k.key == fux_vt::keys::Key::Escape && !k.mods.ctrl && !k.mods.alt
}

impl Models {
    /// What `keymap` holds, as a model.
    fn of(keymap: &Keymap, last: Option<Bound>) -> Models {
        let mut models = Models {
            bound: Vec::new(),
            repeats: Vec::new(),
            last,
        };
        for (keys, entry) in keymap.root.all() {
            match entry {
                Entry::Run(binding) => models.bound.push((keys, binding.clone())),
                Entry::Map(map) if map.repeat => models.repeats.push(keys),
                Entry::Map(_) => {}
            }
        }
        models
    }

    /// Forgets the repeat of each map no binding is in any more.
    fn tidy(&mut self) {
        let bound = &self.bound;
        (self.repeats).retain(|map| {
            bound
                .iter()
                .any(|(keys, _)| keys.len() > map.len() && keys.starts_with(map))
        });
    }

    /// Applies `statement`: whether it was accepted.
    fn apply(&mut self, statement: &Statement) -> bool {
        match statement {
            Statement::Bind { keys, code } => {
                // The code is compiled as it is bound, from the words
                // between code and end.
                let Some(code) = compiled(code) else {
                    return false;
                };
                let Ok(keys) = fux::keymap::keys(keys) else {
                    return false;
                };
                // A key is a binding or a map, never both; and Escape
                // leaves a map.
                let conflict = self.bound.iter().any(|(b, _)| overlap(b, &keys));
                if conflict || keys.iter().skip(1).any(escape) {
                    return false;
                }
                let new = Binding {
                    label: code.text(),
                    code,
                    group: "Other".to_owned(),
                };
                match self.bound.iter_mut().find(|(b, _)| *b == keys) {
                    Some((_, old)) => *old = new,
                    None => self.bound.push((keys.clone(), new)),
                }
                self.last = Some(Bound::Keys(keys));
                true
            }
            Statement::Unbind(keys) => {
                let Ok(keys) = fux::keymap::keys(keys) else {
                    return false;
                };
                let before = self.bound.len();
                self.bound.retain(|(b, _)| !b.starts_with(&keys));
                self.tidy();
                self.bound.len() < before
            }
            Statement::UnbindAll => {
                self.bound.clear();
                self.repeats.clear();
                true
            }
            Statement::MoveKeys { from, to } => {
                let (Ok(from), Ok(to)) = (fux::keymap::keys(from), fux::keymap::keys(to)) else {
                    return false;
                };
                if to.starts_with(&from) || to.iter().skip(1).any(escape) {
                    return false;
                }
                let mut moved = self.clone();
                let under = |keys: &Vec<KeyPress>| keys.starts_with(&from);
                let taken: Vec<_> = moved
                    .bound
                    .iter()
                    .filter(|(b, _)| under(b))
                    .cloned()
                    .collect();
                if taken.is_empty() {
                    return false;
                }
                let repeats: Vec<_> = moved.repeats.iter().filter(|r| under(r)).cloned().collect();
                moved.bound.retain(|(b, _)| !under(b));
                moved.tidy();
                moved.repeats.retain(|r| !under(r));
                let to_it = |keys: &[KeyPress]| -> Vec<KeyPress> {
                    to.iter()
                        .chain(keys.get(from.len()..).unwrap_or_default())
                        .copied()
                        .collect()
                };
                // What `to` holds: a binding there is replaced; a map, or
                // a binding that starts `to`, refuses.
                let held = moved
                    .bound
                    .iter()
                    .any(|(b, _)| b.len() > to.len() && b.starts_with(&to));
                let started = moved
                    .bound
                    .iter()
                    .any(|(b, _)| b.len() < to.len() && to.starts_with(b));
                if held || started {
                    return false;
                }
                moved.bound.retain(|(b, _)| *b != to);
                moved.tidy();
                moved
                    .bound
                    .extend(taken.iter().map(|(b, binding)| (to_it(b), binding.clone())));
                moved.repeats.extend(repeats.iter().map(|r| to_it(r)));
                *self = moved;
                true
            }
            Statement::Label(text) | Statement::Group(text) => {
                let label = matches!(statement, Statement::Label(_));
                let binding = match &self.last {
                    Some(Bound::Keys(keys)) => (self.bound.iter_mut())
                        .find(|(b, _)| b == keys)
                        .map(|(_, binding)| binding),
                    // The fuzz writes no screen's keys.
                    Some(Bound::In(..)) | None => None,
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
            Statement::Repeat(keys) => {
                let Ok(keys) = fux::keymap::keys(keys) else {
                    return false;
                };
                let map = self
                    .bound
                    .iter()
                    .any(|(b, _)| b.len() > keys.len() && b.starts_with(&keys));
                if map && !self.repeats.contains(&keys) {
                    self.repeats.push(keys);
                }
                map
            }
        }
    }

    /// Whether `keymap` holds what the model does, in any order (a map
    /// lists its keys together).
    fn agree(&self, keymap: &Keymap) -> bool {
        let held = Models::of(keymap, None);
        held.bound.len() == self.bound.len()
            && self.bound.iter().all(|b| held.bound.contains(b))
            && held.repeats.len() == self.repeats.len()
            && self.repeats.iter().all(|r| held.repeats.contains(r))
    }
}

/// The code value `code … end` makes of `code`, if it compiles.
fn compiled(code: &str) -> Option<fux_wire::callback::Code> {
    let (_, compiles) = CODES.iter().find(|(c, _)| *c == code)?;
    let tokens = fuxth::token::source("binding", code)
        .ok()
        .filter(|_| *compiles)?;
    let words = tokens.iter().map(|t| t.text.to_string()).collect();
    Some(fux_wire::callback::Code { words })
}

/// The rules every keymap keeps that its types do not: no empty map held
/// by a key, no Escape in a map, no key bound twice in one; and every map's
/// column shows.
fn check_keymap(keymap: &Keymap) {
    fn check_map(keymap: &Keymap, path: &mut Vec<KeyPress>, map: &Map) {
        if !path.is_empty() {
            let column = keymap.column(path, usize::MAX);
            let column = column.unwrap_or_else(|| panic!("no column for {path:?}"));
            assert!(column.selected < column.entries.len().max(1));
            assert!(!column.entries.is_empty(), "an empty map at {path:?}");
        }
        let keys: Vec<(Vec<KeyPress>, &Entry)> = map.all();
        for (i, (keys_i, _)) in keys.iter().enumerate() {
            let again = keys.iter().skip(i + 1).any(|(o, _)| o == keys_i);
            assert!(!again, "two bindings of {keys_i:?} under {path:?}");
            assert!(
                !keys_i.iter().skip(usize::from(path.is_empty())).any(escape),
                "Escape bound in a map: {path:?} {keys_i:?}"
            );
        }
        for (keys, entry) in keys {
            if let ([key], Entry::Map(inner)) = (keys.as_slice(), entry) {
                path.push(*key);
                check_map(keymap, path, inner);
                path.pop();
            }
        }
    }
    check_map(keymap, &mut Vec::new(), &keymap.root);
}

/// The defaults, built once.
fn default_state() -> &'static fux::client_state::ClientState {
    static DEFAULTS: std::sync::OnceLock<fux::client_state::ClientState> =
        std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| fux::script::default_state().expect("the default state initializes"))
}

fn defaults() -> &'static Keymap {
    &default_state().keys
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
    let mut state = default_state().draft();
    state.keys = keymap.clone();
    let mut host = Host::new(
        fux_wire::protocol::Context::default(),
        Some(&mut state),
        &mut out,
    );
    host.last_bound = last;
    let finished = matches!(machine.run(&mut host, BUDGET), Ok(Outcome::Finished));
    let last = host.last_bound.clone();
    *keymap = state.keys;
    (finished, last)
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
        let codes: Vec<&fux_wire::callback::Code> = (self.keymap.root.all().into_iter())
            .filter_map(|(_, entry)| match entry {
                Entry::Run(binding) => Some(&binding.code),
                Entry::Map(_) => None,
            })
            .collect();
        for step in &steps {
            if let Effect::Run(fux::screens::Job { program, .. }) = step {
                // Only a bound code value runs.
                let fux::screens::Program::Code(code) = program else {
                    panic!("{program:?} runs but is no code value");
                };
                assert!(
                    codes.contains(&code) || (read_only && code.words == ["me", "detach"]),
                    "{code:?} runs but is not bound"
                );
            }
        }
        if !read_only && held_before == Held::Normal {
            let expected = match self.keymap.root.get(press) {
                Some(Entry::Map(_)) => {
                    let column = self.keymap.column(&[press], 0);
                    vec![Effect::Open(fux_wire::act::Panel::Column(
                        column.unwrap_or_default(),
                    ))]
                }
                Some(Entry::Run(binding)) => vec![Effect::Run(binding.code.clone().into())],
                None => vec![Effect::Send(fux_wire::act::Act::Key(stroke))],
            };
            assert_eq!(steps, expected, "{press} typed");
        }
        if matches!(self.held, Held::Normal | Held::Prefixed(_)) {
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
                "repeating {path:?}, which is no map that repeats"
            ),
            Held::Normal | Held::Prefixed(_) => {}
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
                // Written as native hints and list-keys write words, they read back.
                let texts: Vec<&str> = tokens.iter().map(|t| &*t.text).collect();
                let quoted: Vec<String> = texts.iter().map(|t| quote(t)).collect();
                let again = fuxth::token::source("again", &quoted.join(" "));
                let again = again.unwrap_or_else(|e| panic!("{quoted:?}: {e}"));
                let again: Vec<&str> = again.iter().map(|t| &*t.text).collect();
                assert_eq!(again, texts, "{text:?}");
                let mut machine = match fux::script::compile(&tokens) {
                    Ok(machine) => machine,
                    Err(failed) => {
                        // A mistake in the program (2), or a refusal (1).
                        let status = failed.code.status().get();
                        assert!(matches!(status, 1 | 2), "{text:?}: {failed}");
                        assert!(!failed.code.name().is_empty(), "{text:?}: {failed}");
                        continue;
                    }
                };
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
