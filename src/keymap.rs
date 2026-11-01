//! A client's keys: its keymap, the command column and repeat modes it
//! shows, and the tables that turn its keys into the actions of fux's own
//! screens. All of it in the client: the server is sent typed actions, and
//! keys only as literal input to a pane.
//!
//! A keymap maps each of its keys to a binding or to another keymap, at any
//! depth, as Emacs's prefix maps do. The root map is the one a client is in
//! while no keys are held: its prefix, `C-b`, is a key there that holds the
//! map of fux's commands, and `C-b t` holds the tab commands in that one.
//!
//! A binding runs fuxth: its code value is compiled and run by the client,
//! fresh, each time its keys are typed, the client itself its `me`. The default
//! bindings are a config too, `script/keys.fux`, run on `Keymap::default`
//! before the user's.
use crate::screens::{Effect, HIDE};
use fux_vt::keys::{Key, KeyPress, Keystroke, Modifiers};
use fux_wire::act::ScreenAct;
use fux_wire::act::{Act, Column, ColumnEntry, Edit, Hint, LineAct, ListAct, Move, Panel};
use fux_wire::callback::Code;
use fux_wire::json::Json;
use fux_wire::words::quote;

/// What a key runs: a code value, what the column says it does, and the
/// group the column lists it under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub code: Code,
    pub label: String,
    pub group: String,
}

impl Binding {
    /// What `bind` makes of `code`: labelled with its words, in the group
    /// listed last.
    pub fn new(code: Code) -> Self {
        Self {
            label: code.text(),
            code,
            group: OTHER_GROUP.to_owned(),
        }
    }

    /// The config line that binds `keys` to it: its label and group too,
    /// unless they are what `bind` gives.
    fn line(&self, keys: &str) -> String {
        let mut line = format!("str {} code {} end bind", quote(keys), self.code.text());
        if self.label != self.code.text() {
            line += &format!("  str {} label", quote(&self.label));
        }
        if self.group != OTHER_GROUP {
            line += &format!("  str {} group", quote(&self.group));
        }
        line
    }
}

/// What a key of a map does: runs a binding, or holds another map of more
/// keys (`C-b t n` is `n` in the map `t` holds in the map `C-b` holds).
/// Never both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Run(Binding),
    Map(Map),
}

/// A keymap: its keys, each bound once, in the order first bound. One that
/// `repeat`s stays the client's map after a key of it runs, until Esc. A
/// map held by a key is never empty: unbinding its last key unbinds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Map {
    keys: Vec<(KeyPress, Entry)>,
    pub repeat: bool,
}

impl Map {
    pub fn get(&self, key: KeyPress) -> Option<&Entry> {
        self.keys.iter().find(|(k, _)| *k == key).map(|(_, e)| e)
    }

    /// The map at `path` below this one; this one if `path` is empty.
    pub fn map(&self, path: &[KeyPress]) -> Option<&Map> {
        match path.split_first() {
            None => Some(self),
            Some((key, rest)) => match self.get(*key)? {
                Entry::Map(map) => map.map(rest),
                Entry::Run(_) => None,
            },
        }
    }

    /// What `path` below this map is.
    fn entry_mut(&mut self, path: &[KeyPress]) -> Option<&mut Entry> {
        let (key, rest) = path.split_first()?;
        let entry = (self.keys.iter_mut()).find(|(k, _)| k == key)?;
        match (&mut entry.1, rest.is_empty()) {
            (entry, true) => Some(entry),
            (Entry::Map(map), false) => map.entry_mut(rest),
            (Entry::Run(_), false) => None,
        }
    }

    /// Every key below it, depth first, a map before its keys: their keys
    /// from here, and what each is.
    pub fn all(&self) -> Vec<(Vec<KeyPress>, &Entry)> {
        let mut all = Vec::new();
        for (key, entry) in &self.keys {
            all.push((vec![*key], entry));
            if let Entry::Map(map) = entry {
                let below = map.all().into_iter();
                all.extend(below.map(|(keys, e)| ([*key].into_iter().chain(keys).collect(), e)));
            }
        }
        all
    }

    /// Its first binding, depth first, whose group titles it.
    fn first(&self) -> Option<&Binding> {
        self.all().into_iter().find_map(|(_, entry)| match entry {
            Entry::Run(binding) => Some(binding),
            Entry::Map(_) => None,
        })
    }

    /// Puts `entry` at the `rest` of `keys` below this map, replacing the
    /// binding there, the maps it needs made. Keys that hold a map, or that
    /// start with keys that run a binding, are refused.
    fn put(&mut self, keys: &[KeyPress], rest: &[KeyPress], entry: Entry) -> Result<(), String> {
        let Some((key, after)) = rest.split_first() else {
            return Err("no keys given".to_owned());
        };
        let Some((_, there)) = self.keys.iter_mut().find(|(k, _)| k == key) else {
            let entry = match after.is_empty() {
                true => entry,
                false => {
                    let mut map = Map::default();
                    map.put(keys, after, entry)?;
                    Entry::Map(map)
                }
            };
            self.keys.push((*key, entry));
            return Ok(());
        };
        match (there, after.is_empty()) {
            (Entry::Map(map), false) => map.put(keys, after, entry),
            (Entry::Map(_), true) => Err(format!(
                "{} holds a map of more keys; unbind it first",
                keys_text(keys)
            )),
            (there @ Entry::Run(_), true) => {
                *there = entry;
                Ok(())
            }
            (Entry::Run(binding), false) => Err(format!(
                "{} runs {}, so it cannot start {}; unbind it first",
                keys_text(keys.strip_suffix(after).unwrap_or(keys)),
                binding.label,
                keys_text(keys)
            )),
        }
    }

    /// Takes what `keys` below this map run or hold, unbinding a map it
    /// leaves empty.
    fn take(&mut self, keys: &[KeyPress]) -> Option<Entry> {
        let (key, rest) = keys.split_first()?;
        let entry = &mut self.keys.iter_mut().find(|(k, _)| k == key)?.1;
        let taken = match (entry, rest.is_empty()) {
            (entry, true) => std::mem::replace(entry, Entry::Map(Map::default())),
            (Entry::Map(map), false) => map.take(rest)?,
            (Entry::Run(_), false) => return None,
        };
        // What was taken goes, and a map it left empty.
        (self.keys).retain(|(_, e)| !matches!(e, Entry::Map(map) if map.keys.is_empty()));
        Some(taken)
    }
}

/// Keys as they are written: `C-b t n`.
pub fn keys_text(keys: &[KeyPress]) -> String {
    keys.iter()
        .map(KeyPress::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A key's name, read: `S-` on a letter is its upper case, as a character
/// carries its own shift.
pub fn key(word: &str) -> Result<KeyPress, String> {
    (word.parse()).map_err(|_| format!("{word:?} is no key's name"))
}

/// Keys from their names, separated by spaces: `C-b t n`.
pub fn keys(words: &str) -> Result<Vec<KeyPress>, String> {
    let keys: Vec<KeyPress> = words
        .split_whitespace()
        .map(key)
        .collect::<Result<_, _>>()?;
    (!keys.is_empty())
        .then_some(keys)
        .ok_or_else(|| "no keys given".to_owned())
}

/// A client's keymap and its screens' keys; empty until bundled fuxth
/// supplies them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    /// The map the client is in while no keys are held.
    pub root: Map,
    /// The keys of fux's own screens.
    pub modes: Modes,
}

/// The group of bindings that gave none, listed last.
const OTHER_GROUP: &str = "Other";

impl Keymap {
    /// Binds `keys` from the root to `binding`.
    pub fn bind(&mut self, keys: &[KeyPress], binding: Binding) -> Result<(), String> {
        self.put(keys, Entry::Run(binding))
    }

    /// Puts `entry` at `keys`, or refuses and changes nothing.
    fn put(&mut self, keys: &[KeyPress], entry: Entry) -> Result<(), String> {
        let escape = |k: &KeyPress| k.plain_key() == Some(Key::Escape);
        if keys.iter().skip(1).any(escape) {
            return Err("Escape leaves a map of keys; it cannot be bound in one".to_owned());
        }
        self.root.put(keys, keys, entry)
    }

    /// Unbinds `keys`: a binding, or a whole map.
    pub fn unbind(&mut self, keys: &[KeyPress]) -> Result<(), String> {
        let unbound = self.root.take(keys).is_some();
        unbound
            .then_some(())
            .ok_or_else(|| format!("{} is not bound", keys_text(keys)))
    }

    /// What `from` runs or holds, `to` does instead, and `from` nothing:
    /// `str C-b str C-a move-keys` moves the prefix. All or nothing.
    pub fn move_keys(&mut self, from: &[KeyPress], to: &[KeyPress]) -> Result<(), String> {
        if to.starts_with(from) {
            return Err(format!("{} is in {}", keys_text(to), keys_text(from)));
        }
        let mut moved = self.clone();
        let entry =
            (moved.root.take(from)).ok_or_else(|| format!("{} is not bound", keys_text(from)))?;
        moved.put(to, entry)?;
        *self = moved;
        Ok(())
    }

    pub fn unbind_all(&mut self) {
        *self = Keymap::default();
    }

    /// The binding `keys` run.
    pub fn binding_mut(&mut self, keys: &[KeyPress]) -> Option<&mut Binding> {
        match self.root.entry_mut(keys)? {
            Entry::Run(binding) => Some(binding),
            Entry::Map(_) => None,
        }
    }

    /// The map `keys` hold repeats: it stays after a key of it runs, until
    /// Esc.
    pub fn set_repeat(&mut self, keys: &[KeyPress]) -> Result<(), String> {
        match self.root.entry_mut(keys) {
            Some(Entry::Map(map)) => {
                map.repeat = true;
                Ok(())
            }
            Some(Entry::Run(_)) | None => Err(format!("{} holds no map", keys_text(keys))),
        }
    }

    /// The first key of the root that holds a map: the prefix, which a
    /// client that only watches detaches with.
    fn prefix(&self) -> Option<KeyPress> {
        (self.root.keys.iter())
            .find_map(|(key, entry)| matches!(entry, Entry::Map(_)).then_some(*key))
    }

    /// Whether `key` holds a map at the root, as the prefix does.
    pub fn leads(&self, key: KeyPress) -> bool {
        matches!(self.root.get(key), Some(Entry::Map(_)))
    }

    /// The column for the map at `path`, `selected` chosen: its entries
    /// grouped, groups in the order their keys were first bound and `Other`
    /// last.
    pub fn column(&self, path: &[KeyPress], selected: usize) -> Option<Column> {
        let map = self.root.map(path)?;
        let mut entries: Vec<ColumnEntry> = (map.keys.iter())
            .filter_map(|(key, entry)| {
                // A map is titled, and listed, by its first binding's
                // group: `t` is `Tabs…`.
                let (label, group, map) = match entry {
                    Entry::Run(binding) => (binding.label.clone(), binding.group.clone(), false),
                    Entry::Map(inner) => {
                        let group = inner.first()?.group.clone();
                        (group.clone(), group, true)
                    }
                };
                Some(ColumnEntry {
                    key: *key,
                    label,
                    group,
                    map,
                })
            })
            .collect();
        let mut groups: Vec<String> = Vec::new();
        for entry in &entries {
            if !groups.contains(&entry.group) && entry.group != OTHER_GROUP {
                groups.push(entry.group.clone());
            }
        }
        groups.push(OTHER_GROUP.to_owned());
        entries.sort_by_key(|e| groups.iter().position(|g| *g == e.group));
        let selected = selected.min(entries.len().saturating_sub(1));
        Some(Column {
            keys: keys_text(path),
            entries,
            selected,
        })
    }

    /// A repeat mode's bar for the map at `path`, if it repeats and binds
    /// keys: `RESIZE  h j k l · Esc`.
    pub fn repeat_bar(&self, path: &[KeyPress]) -> Option<String> {
        let map = self.root.map(path).filter(|map| map.repeat)?;
        // Its keys that run, titled by the first one's group.
        let mut keys = Vec::new();
        let mut title = None;
        for (key, entry) in &map.keys {
            if let Entry::Run(binding) = entry {
                keys.push(key.to_string());
                title.get_or_insert(binding.group.as_str());
            }
        }
        Some(format!(
            "{}  {} · Esc",
            title?.to_uppercase(),
            keys.join(" ")
        ))
    }

    /// Every binding, as `list-keys` shows them: config lines that make
    /// them again, the maps that repeat said after them. Not the config's
    /// definitions, which a binding may call.
    pub fn describe(&self) -> String {
        let all = self.root.all();
        let bound = all.iter().filter_map(|(keys, entry)| match entry {
            Entry::Run(binding) => Some(format!("{}\n", binding.line(&keys_text(keys)))),
            Entry::Map(_) => None,
        });
        let repeats = all.iter().filter_map(|(keys, entry)| match entry {
            Entry::Map(map) if map.repeat => {
                Some(format!("str {} repeat\n", quote(&keys_text(keys))))
            }
            Entry::Run(_) | Entry::Map(_) => None,
        });
        std::iter::once("unbind-all\n".to_owned())
            .chain(bound)
            .chain(repeats)
            .chain([self.modes.lines()])
            .collect()
    }

    /// The `keys` record: the keys a client holding `at` has typed into
    /// the map it is in, and every key it binds, depth first, each map
    /// before its keys, then its screens' keys: a list, not a tree, so its
    /// depth is the keymap's own.
    pub fn record(&self, at: &[KeyPress]) -> Json {
        let text = |keys: &[KeyPress]| Json::str(keys_text(keys));
        let mut keymap: Vec<Json> = (self.root.all().into_iter())
            .map(|(keys, entry)| match entry {
                Entry::Run(binding) => Json::object(vec![
                    ("kind", Json::str("binding")),
                    ("keys", text(&keys)),
                    ("code", Json::str(binding.code.text())),
                    ("label", Json::str(&binding.label)),
                    ("group", Json::str(&binding.group)),
                ]),
                Entry::Map(map) => Json::object(vec![
                    ("kind", Json::str("map")),
                    ("keys", text(&keys)),
                    ("repeat", Json::Bool(map.repeat)),
                ]),
            })
            .collect();
        keymap.extend(self.modes.record());
        Json::object(vec![("at", text(at)), ("keymap", Json::Array(keymap))])
    }
}

/// What the client's keys are in the middle of, of its own: the map it is
/// in, held at the keys typed into it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Held {
    /// At the root.
    #[default]
    Normal,
    /// The column is shown, of the map at `path`.
    Column {
        path: Vec<KeyPress>,
        selected: usize,
    },
    /// A repeat mode: the map at `path` repeats, and a key of it ran.
    Repeat { path: Vec<KeyPress> },
    /// A client that only watches typed a key that holds a map.
    Prefixed(KeyPress),
}

impl Held {
    /// The keys typed into the map the client is in.
    pub fn at(&self) -> &[KeyPress] {
        match self {
            Held::Normal => &[],
            Held::Column { path, .. } | Held::Repeat { path } => path,
            Held::Prefixed(key) => std::slice::from_ref(key),
        }
    }

    /// A key typed at the client's terminal while none of its screens is
    /// open (`crate::screens`): what it leads to. `read_only`: it only
    /// watches, and the prefix then `d` detaches it.
    pub fn press(&mut self, keymap: &Keymap, stroke: Keystroke, read_only: bool) -> Vec<Effect> {
        let press = stroke.press;
        if read_only {
            return self.watching(keymap, press);
        }
        match std::mem::take(self) {
            Held::Normal | Held::Prefixed(_) => match keymap.root.get(press) {
                Some(Entry::Run(binding)) => vec![Effect::Run(binding.code.clone().into())],
                Some(Entry::Map(_)) => self.open(keymap, vec![press], 0),
                None => vec![Effect::Send(Act::Key(stroke))],
            },
            Held::Column { path, selected } => self.in_column(keymap, path, selected, stroke),
            Held::Repeat { path } => self.repeating(keymap, path, press),
        }
    }

    /// The column for the map at `path`, shown.
    fn open(&mut self, keymap: &Keymap, path: Vec<KeyPress>, selected: usize) -> Vec<Effect> {
        match self.column(keymap, path, selected) {
            Some(panel) => vec![Effect::Open(panel)],
            None => vec![HIDE],
        }
    }

    fn column(&mut self, keymap: &Keymap, path: Vec<KeyPress>, selected: usize) -> Option<Panel> {
        let column = keymap.column(&path, selected)?;
        let selected = column.selected;
        *self = Held::Column { path, selected };
        Some(Panel::Column(column))
    }

    /// `press` typed in the map at `path`: its binding runs, a repeat mode
    /// starting if the map repeats; or the map it holds opens; or, unbound,
    /// the column stays, saying so.
    fn follow(&mut self, keymap: &Keymap, path: Vec<KeyPress>, press: KeyPress) -> Vec<Effect> {
        let map = keymap.root.map(&path);
        match map.and_then(|m| m.get(press)) {
            Some(Entry::Run(binding)) => {
                let shown = match keymap.repeat_bar(&path) {
                    Some(bar) => {
                        *self = Held::Repeat { path };
                        Effect::Open(Panel::Repeat(bar))
                    }
                    None => {
                        *self = Held::Normal;
                        HIDE
                    }
                };
                vec![shown, Effect::Run(binding.code.clone().into())]
            }
            Some(Entry::Map(_)) => {
                let mut deeper = path;
                deeper.push(press);
                self.open(keymap, deeper, 0)
            }
            None => {
                let mut keys = path.clone();
                keys.push(press);
                *self = Held::Column { path, selected: 0 };
                let text = format!("{} is not bound", keys_text(&keys));
                vec![Effect::Send(Act::Notice { text, error: true })]
            }
        }
    }

    fn in_column(
        &mut self,
        keymap: &Keymap,
        path: Vec<KeyPress>,
        selected: usize,
        stroke: Keystroke,
    ) -> Vec<Effect> {
        let press = stroke.press;
        if press.plain_key() == Some(Key::Escape) {
            return vec![HIDE];
        }
        let map = keymap.root.map(&path);
        if map.is_some_and(|m| m.get(press).is_some()) {
            return self.follow(keymap, path, press);
        }
        if path.first() == Some(&press) {
            // The key that led here, unbound in it, goes to the pane.
            return vec![HIDE, Effect::Send(Act::Key(stroke))];
        }
        let count = keymap.column(&path, 0).map_or(0, |c| c.entries.len());
        let last = count.saturating_sub(1);
        // A list's moves, but by its keys that are not letters, which are
        // bindings here.
        let unbound = press.mods == Modifiers::NONE && !matches!(press.key, Key::Char(_));
        if let Some(ListAct::Move(moved)) = keymap.modes.list.get(press).filter(|_| unbound) {
            return match self.column(keymap, path, moved.from(selected, last)) {
                Some(panel) => vec![Effect::Screen(ScreenAct::Show(panel))],
                None => vec![HIDE],
            };
        }
        if press.key == Key::Enter && press.mods == Modifiers::NONE {
            let chosen = keymap.column(&path, selected).and_then(|c| {
                let entry = c.entries.into_iter().nth(c.selected)?;
                Some(entry.key)
            });
            return match chosen {
                Some(key) => self.follow(keymap, path, key),
                None => vec![HIDE],
            };
        }
        self.follow(keymap, path, press)
    }

    fn repeating(&mut self, keymap: &Keymap, path: Vec<KeyPress>, press: KeyPress) -> Vec<Effect> {
        let bound = keymap.root.map(&path).and_then(|m| m.get(press));
        if bound.is_none() && path.first() == Some(&press) {
            // The key that led here opens its column again.
            return self.open(keymap, vec![press], 0);
        }
        let plain = !press.mods.ctrl && !press.mods.alt;
        let leaves = press.key == Key::Escape || (press.key == Key::Enter && bound.is_none());
        if plain && leaves {
            return vec![HIDE];
        }
        match bound {
            Some(Entry::Run(binding)) => {
                let code = binding.code.clone();
                *self = Held::Repeat { path };
                vec![Effect::Run(code.into())]
            }
            Some(Entry::Map(_)) => {
                let mut deeper = path;
                deeper.push(press);
                self.open(keymap, deeper, 0)
            }
            None => {
                let text = format!("{press} is not one of this mode's keys");
                vec![HIDE, Effect::Send(Act::Notice { text, error: false })]
            }
        }
    }

    fn watching(&mut self, keymap: &Keymap, press: KeyPress) -> Vec<Effect> {
        let prefixed = matches!(std::mem::take(self), Held::Prefixed(_));
        if prefixed && press == KeyPress::char('d') {
            let detach = ["me", "detach"].map(String::from).into();
            return vec![Effect::Run(Code { words: detach }.into())];
        }
        if !prefixed && keymap.leads(press) {
            *self = Held::Prefixed(press);
            return Vec::new();
        }
        vec![Effect::Send(watching(keymap))]
    }
}

/// What a client that only watches is told of a key or paste it takes no
/// further: its own way out.
pub fn watching(keymap: &Keymap) -> Act {
    let text = keymap.prefix().map_or_else(
        || "watching only".to_owned(),
        |p| format!("watching only: {p} d detaches"),
    );
    Act::Notice { text, error: false }
}

/// The keys of fux's own screens, from the config as the bindings are
/// (`bind-in`): each a key to one of its screen's actions, and the hint
/// that names it there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub list: Table<ListAct>,
    pub line: Table<LineAct>,
}

/// One of fux's own screens, as `bind-in` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    List,
    Line,
}

impl Modal {
    pub const ALL: &[(&str, Modal)] = &[("list", Modal::List), ("line", Modal::Line)];
}

impl Modes {
    pub fn table(&mut self, modal: Modal) -> &mut dyn Bindable {
        match modal {
            Modal::List => &mut self.list,
            Modal::Line => &mut self.line,
        }
    }

    fn tables(&self) -> impl Iterator<Item = (&'static str, &dyn Bindable)> {
        let tables: [&dyn Bindable; 2] = [&self.list, &self.line];
        (Modal::ALL.iter().map(|(name, _)| *name)).zip(tables)
    }

    /// Every key, as config lines that bind it again.
    fn lines(&self) -> String {
        self.tables()
            .map(|(name, table)| table.lines(name))
            .collect()
    }

    /// Every key, as the `keys` record lists them.
    fn record(&self) -> Vec<Json> {
        self.tables()
            .flat_map(|(name, table)| table.record(name))
            .collect()
    }
}

/// The actions of one of fux's screens, by the names `bind-in` reads; and
/// what Escape does there, always, so that no screen holds a key it cannot
/// leave.
pub trait Named: Clone + PartialEq + 'static {
    const NAMES: &'static [(&'static str, Self)];
    const ESCAPE: Self;

    /// The code value it runs, if one.
    fn source(&self) -> Option<&Code> {
        None
    }

    fn name(&self) -> &'static str {
        (Self::NAMES.iter())
            .find(|(_, a)| a == self)
            .map_or("", |(name, _)| name)
    }
}

/// A screen's keys, each once, in the order bound: its action, and the
/// hint that names it, if any. Letter case is exact; Escape is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table<A>(Vec<(KeyPress, A, Option<String>)>);

impl<A> Default for Table<A> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<A: Named> Table<A> {
    /// What `press` does here: a named key's, with any modifiers, its own
    /// unless bound with them.
    pub fn get(&self, press: KeyPress) -> Option<A> {
        if press.key == Key::Escape {
            return Some(A::ESCAPE);
        }
        let find = |press: KeyPress| (self.0.iter()).find(|(k, ..)| *k == press);
        let named = (!matches!(press.key, Key::Char(_))).then(|| KeyPress::plain(press.key));
        let found = find(press).or_else(|| named.and_then(find));
        found.map(|(_, a, _)| a.clone())
    }

    /// The hints a screen shows of its keys: each label once, the keys it
    /// names together, in the order bound; Escape with the keys of its
    /// action, else last, named by it.
    pub fn hints(&self) -> Vec<Hint> {
        let escape = (self.0.iter()).find(|(_, a, l)| *a == A::ESCAPE && l.is_some());
        let label = escape.and_then(|(.., l)| l.clone());
        let escape = (
            KeyPress::plain(Key::Escape),
            label.unwrap_or(A::ESCAPE.name().to_owned()),
        );
        let labelled = (self.0.iter()).filter_map(|(k, _, l)| Some((*k, l.clone()?)));
        let mut hints: Vec<Hint> = Vec::new();
        for (key, label) in labelled.chain([escape]) {
            match hints.iter_mut().find(|h| h.label == label) {
                Some(hint) => hint.keys = format!("{} {key}", hint.keys),
                None => hints.push(Hint {
                    keys: key.to_string(),
                    label,
                }),
            }
        }
        hints
    }
}

impl Table<ListAct> {
    pub fn bind_action(
        &mut self,
        key: KeyPress,
        action: fux_wire::callback::Callback,
    ) -> Result<(), String> {
        if key.key == Key::Escape {
            return Err("Escape leaves every screen of fux's own; it cannot be bound".to_owned());
        }
        let bound = (key, ListAct::Action(action), None);
        match self.0.iter_mut().find(|(k, ..)| *k == key) {
            Some(was) => *was = bound,
            None => self.0.push(bound),
        }
        Ok(())
    }
}

/// A screen's table, whatever its actions: what a config's words change.
pub trait Bindable {
    /// Binds `key` to the action named `name`, in place of what it did.
    fn bind(&mut self, key: KeyPress, name: &str) -> Result<(), String>;
    fn unbind(&mut self, key: KeyPress) -> Result<(), String>;
    /// The hint of `key`'s action, if it is bound.
    fn label(&mut self, key: KeyPress) -> Option<&mut Option<String>>;
    /// Its keys as config lines, for the screen named `modal`.
    fn lines(&self, modal: &str) -> String;
    /// Its keys as the `keys` record lists them: each of `kind` the screen
    /// named `modal`, with its action's name or its code.
    fn record(&self, modal: &'static str) -> Vec<Json>;
}

impl<A: Named> Bindable for Table<A> {
    fn bind(&mut self, key: KeyPress, name: &str) -> Result<(), String> {
        if key.key == Key::Escape {
            return Err("Escape leaves every screen of fux's own; it cannot be bound".to_owned());
        }
        let Some((_, act)) = A::NAMES.iter().find(|(n, _)| *n == name) else {
            let names: Vec<&str> = A::NAMES.iter().map(|(n, _)| *n).collect();
            return Err(format!(
                "{name:?} is not {}",
                fux_wire::words::listed(&names, "or")
            ));
        };
        // A key bound again keeps its place, as a binding's does.
        let bound = (key, act.clone(), None);
        match self.0.iter_mut().find(|(k, ..)| *k == bound.0) {
            Some(was) => *was = bound,
            None => self.0.push(bound),
        }
        Ok(())
    }

    fn unbind(&mut self, key: KeyPress) -> Result<(), String> {
        let before = self.0.len();
        self.0.retain(|(k, ..)| *k != key);
        let unbound = self.0.len() != before;
        unbound
            .then_some(())
            .ok_or_else(|| format!("{key} is not bound there"))
    }

    fn label(&mut self, key: KeyPress) -> Option<&mut Option<String>> {
        (self.0.iter_mut())
            .find(|(k, ..)| *k == key)
            .map(|(.., label)| label)
    }

    fn lines(&self, modal: &str) -> String {
        let line = |(key, act, label): &(KeyPress, A, Option<String>)| {
            let key = quote(&key.to_string());
            let label = label.as_ref().map(|l| format!("  str {} label", quote(l)));
            let label = label.unwrap_or_default();
            match act.source() {
                Some(code) => format!("str {key} code {} end bind-action{label}\n", code.text()),
                None => format!("str {modal} str {key} str {} bind-in{label}\n", act.name()),
            }
        };
        self.0.iter().map(line).collect()
    }

    fn record(&self, modal: &'static str) -> Vec<Json> {
        let text = |text: Option<String>| text.map_or(Json::Null, Json::String);
        let entry = |(key, act, label): &(KeyPress, A, Option<String>)| {
            let code = act.source().map(Code::text);
            Json::object(vec![
                ("kind", Json::str(modal)),
                ("keys", Json::str(key.to_string())),
                (
                    "action",
                    text(code.is_none().then(|| act.name().to_owned())),
                ),
                ("code", text(code)),
                ("label", text(label.clone())),
            ])
        };
        self.0.iter().map(entry).collect()
    }
}

impl Named for ListAct {
    #[rustfmt::skip]
    const NAMES: &'static [(&'static str, Self)] = &[
        ("up", ListAct::Move(Move::Up)), ("down", ListAct::Move(Move::Down)),
        ("page-up", ListAct::Move(Move::PageUp)), ("page-down", ListAct::Move(Move::PageDown)),
        ("first", ListAct::Move(Move::First)), ("last", ListAct::Move(Move::Last)),
        ("choose", ListAct::Choose), ("cancel", ListAct::Cancel),
    ];
    const ESCAPE: Self = ListAct::Cancel;
    fn source(&self) -> Option<&Code> {
        match self {
            Self::Action(action) => Some(&action.code),
            Self::Move(_) | Self::Choose | Self::Cancel => None,
        }
    }
}

impl Named for LineAct {
    #[rustfmt::skip]
    const NAMES: &'static [(&'static str, Self)] = &[
        ("left", LineAct::Edit(Edit::Left)), ("right", LineAct::Edit(Edit::Right)),
        ("home", LineAct::Edit(Edit::Home)), ("end", LineAct::Edit(Edit::End)),
        ("backspace", LineAct::Edit(Edit::Backspace)), ("delete", LineAct::Edit(Edit::Delete)),
        ("submit", LineAct::Submit), ("cancel", LineAct::Cancel),
    ];
    const ESCAPE: Self = LineAct::Cancel;
}

impl Table<LineAct> {
    /// What `press` does to a line being typed: its key's action; else the
    /// character it types (with Ctrl or Alt, a letter types none).
    pub fn typed(&self, press: KeyPress) -> Option<LineAct> {
        self.get(press).or_else(|| {
            let Key::Char(c) = press.key else { return None };
            let typed = !press.mods.ctrl && !press.mods.alt && !c.is_control();
            typed.then(|| LineAct::Edit(Edit::Insert(c.to_string())))
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_keymap_has_no_native_prefix() {
        let keys = super::Keymap::default();
        let mut held = super::Held::default();
        let stroke = super::KeyPress::new(super::Key::Char('b'), super::Modifiers::CTRL).into();
        assert!(matches!(
            held.press(&keys, stroke, false).as_slice(),
            [crate::screens::Effect::Send(fux_wire::act::Act::Key(_))]
        ));
        assert_eq!(held, super::Held::Normal);
    }

    use super::*;
    use fux_wire::act::PAGE;

    fn defaults() -> Result<Keymap, String> {
        crate::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())
    }

    fn stroke(name: &str) -> Result<Keystroke, String> {
        key(name).map(Keystroke::from)
    }

    #[test]
    fn modal_keys_keep_case_in_actions_labels_and_unbinding() -> Result<(), String> {
        fn check<A: Named>(lower: &str, upper: &str) -> Result<(), String> {
            let mut table = Table::<A>::default();
            table.bind(key("g")?, lower)?;
            table.bind(key("S-g")?, upper)?;
            *table.label(key("g")?).ok_or("g label")? = Some("lower".into());
            *table.label(key("G")?).ok_or("G label")? = Some("upper".into());
            assert_eq!(table.get(key("g")?).map(|a| a.name()), Some(lower));
            assert_eq!(table.get(key("G")?).map(|a| a.name()), Some(upper));
            let hints = table.hints();
            assert!(
                hints
                    .iter()
                    .any(|h| h.keys.split_whitespace().any(|k| k == "g") && h.label == "lower")
            );
            assert!(hints.iter().any(|h| h.keys == "G" && h.label == "upper"));
            table.unbind(key("g")?)?;
            assert!(table.get(key("g")?).is_none());
            assert_eq!(table.get(key("G")?).map(|a| a.name()), Some(upper));
            Ok(())
        }
        check::<ListAct>("up", "down")?;
        check::<LineAct>("home", "end")
    }

    #[test]
    fn a_shifted_letter_is_its_upper_case() {
        assert_eq!(key("S-v").map(|k| k.key), Ok(Key::Char('V')));
        assert_eq!(key("s-M-v").map(|k| k.key), Ok(Key::Char('V')));
        assert_eq!(key("S-1").map(|k| k.key), Ok(Key::Char('1')));
    }

    #[test]
    fn the_defaults_bind_and_list_in_groups() -> Result<(), String> {
        let keymap = defaults()?;
        let column = keymap.column(&keys("C-b")?, 0).ok_or("a column")?;
        let mut groups: Vec<String> = column.entries.into_iter().map(|e| e.group).collect();
        groups.dedup();
        let order = [
            "Panes",
            "Focus",
            "Tabs",
            "Workspaces",
            "Session",
            "Resize",
            "Pane actions",
            "Move",
        ];
        assert_eq!(groups, order, "in the order keys.fux binds them");
        assert_eq!(
            keymap.repeat_bar(&keys("C-b r")?).as_deref(),
            Some("RESIZE  h j k l · Esc")
        );
        Ok(())
    }

    #[test]
    fn the_prefix_opens_the_column_a_key_runs_its_binding() -> Result<(), String> {
        let keymap = defaults()?;
        let mut held = Held::default();
        let steps = held.press(&keymap, stroke("C-b")?, false);
        assert!(matches!(steps.as_slice(), [Effect::Open(Panel::Column(_))]));
        let steps = held.press(&keymap, stroke("z")?, false);
        assert_eq!(
            steps,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(crate::testing::code("me focused zoom").into())
            ]
        );
        assert_eq!(held, Held::Normal);
        let steps = held.press(&keymap, stroke("x")?, false);
        assert!(
            matches!(steps.as_slice(), [Effect::Send(Act::Key(_))]),
            "an unbound key is the pane's"
        );
        Ok(())
    }

    /// A map holds maps at any depth: a key in one that repeats runs and
    /// stays in it, the keys typed into it held, until Escape.
    #[test]
    fn a_map_two_deep_repeats_until_escape() -> Result<(), String> {
        let mut keymap = defaults()?;
        let zoom = || Binding::new(crate::testing::code("me focused zoom"));
        keymap.bind(&keys("C-b y u i")?, zoom())?;
        keymap.set_repeat(&keys("C-b y u")?)?;
        let mut held = Held::default();
        for name in ["C-b", "y", "u"] {
            held.press(&keymap, stroke(name)?, false);
        }
        assert_eq!(held.at(), keys("C-b y u")?);
        for _ in 0..2 {
            let steps = held.press(&keymap, stroke("i")?, false);
            assert!(
                steps.contains(&Effect::Run(zoom().code.into())),
                "{steps:?}"
            );
            assert!(matches!(held, Held::Repeat { .. }));
        }
        held.press(&keymap, stroke("Escape")?, false);
        assert_eq!(held, Held::Normal);
        Ok(())
    }

    #[test]
    fn a_watching_client_only_detaches() -> Result<(), String> {
        let keymap = defaults()?;
        let mut held = Held::default();
        let steps = held.press(&keymap, stroke("x")?, true);
        assert!(matches!(
            steps.as_slice(),
            [Effect::Send(Act::Notice { .. })]
        ));
        held.press(&keymap, stroke("C-b")?, true);
        let steps = held.press(&keymap, stroke("d")?, true);
        assert_eq!(
            steps,
            [Effect::Run(crate::testing::code("me detach").into())]
        );
        Ok(())
    }

    /// The column's own keys move its selection within its bindings; a
    /// letter is a binding's, and Enter runs the selected one, the group
    /// `Other` listed last.
    #[test]
    fn the_column_scrolls_within_its_bindings() -> Result<(), String> {
        let mut keymap = defaults()?;
        let new_tab = || crate::testing::code("opts new-tab drop");
        keymap.bind(&keys("C-b y")?, Binding::new(new_tab()))?;
        let prefix = keys("C-b")?;
        let entries = keymap
            .column(&prefix, 0)
            .map(|c| c.entries)
            .unwrap_or_default();
        let last = entries.len().saturating_sub(1);
        assert!(
            entries.last().is_some_and(|e| e.group == OTHER_GROUP),
            "{entries:?}"
        );
        assert_eq!(
            entries.first().map(|e| e.key.to_string()).as_deref(),
            Some("v"),
            "Panes come first"
        );
        let mut held = Held::default();
        let mut press = |name: &str| -> Result<Vec<Effect>, String> {
            Ok(held.press(&keymap, stroke(name)?, false))
        };
        press("C-b")?;
        let selected = |steps: Vec<Effect>| match steps.as_slice() {
            [
                Effect::Screen(ScreenAct::Show(Panel::Column(column)))
                | Effect::Open(Panel::Column(column)),
            ] => Some(column.selected),
            _ => None,
        };
        for _ in 0..last {
            press("Down")?;
        }
        assert_eq!(
            selected(press("Down")?),
            Some(last),
            "Down stops at the last entry"
        );
        assert_eq!(selected(press("Home")?), Some(0));
        assert_eq!(selected(press("PageDown")?), Some(PAGE.min(last)));
        assert_eq!(
            selected(press("Up")?),
            Some(PAGE.min(last).saturating_sub(1))
        );
        assert_eq!(selected(press("End")?), Some(last));
        assert_eq!(
            press("Enter")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(new_tab().into())
            ]
        );
        press("C-b")?;
        assert_eq!(
            press("j")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run(crate::testing::code("me-focused str down step").into())
            ]
        );
        Ok(())
    }

    #[test]
    fn list_keys_shows_maps_their_repeats_and_keys_at_the_root() -> Result<(), String> {
        let mut keymap = Keymap::default();
        let bind = |keymap: &mut Keymap, keys_text: &str, code: &str| {
            keymap.bind(&keys(keys_text)?, Binding::new(crate::testing::code(code)))
        };
        bind(&mut keymap, "C-b g n", "opts new-tab drop")?;
        bind(&mut keymap, "C-b y l", "me focused str right 1 move-border")?;
        keymap.set_repeat(&keys("C-b y")?)?;
        bind(&mut keymap, "M-t", "me focused zoom")?;
        assert_eq!(
            keymap.describe(),
            "unbind-all\nstr 'C-b g n' code opts new-tab drop end bind\nstr 'C-b y l' code me focused str right 1 move-border end bind\nstr M-t code me focused zoom end bind\nstr 'C-b y' repeat\n"
        );
        Ok(())
    }

    /// A key is a binding or a map, never both, and moving keys moves a
    /// map whole, or refuses and changes nothing.
    #[test]
    fn keys_bind_a_binding_or_a_map_and_move_whole() -> Result<(), String> {
        let mut keymap = defaults()?;
        let zoom = || Binding::new(crate::testing::code("me focused zoom"));
        assert!(
            keymap.bind(&keys("C-b t")?, zoom()).is_err(),
            "t holds a map"
        );
        assert!(
            keymap.bind(&keys("C-b z q")?, zoom()).is_err(),
            "z runs a binding"
        );
        assert!(keymap.bind(&keys("C-b t Escape")?, zoom()).is_err());
        let before = keymap.clone();
        assert!(keymap.move_keys(&keys("C-b")?, &keys("C-b q")?).is_err());
        assert!(keymap.move_keys(&keys("C-b")?, &keys("C-b z")?).is_err());
        assert!(keymap.move_keys(&keys("C-a")?, &keys("C-b z")?).is_err());
        assert_eq!(keymap, before);
        keymap.move_keys(&keys("C-b")?, &keys("C-a")?)?;
        assert_eq!(
            keymap.column(&keys("C-a")?, 0),
            before.column(&keys("C-b")?, 0).map(|c| Column {
                keys: "C-a".into(),
                ..c
            })
        );
        assert!(keymap.root.get(key("C-b")?).is_none());
        keymap.unbind(&keys("C-a t m h")?)?;
        keymap.unbind(&keys("C-a t m l")?)?;
        assert!(
            keymap.root.map(&keys("C-a t m")?).is_none(),
            "an emptied map goes"
        );
        Ok(())
    }
}
