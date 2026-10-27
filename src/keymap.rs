//! A client's keys: its prefix and bindings, the command column and repeat
//! modes it shows, and the tables that turn its keys into the actions of
//! fux's own screens. All of it in the client: the server is sent typed
//! actions, and keys only as literal input to a pane.
//!
//! A binding runs fuxth: its code is compiled and run by the client, fresh,
//! each time its keys are typed, the client itself its `me`. The default
//! bindings are a config too, `script/keys.fux`, run on `Keymap::empty`
//! before the user's.
use crate::act::ScreenAct;
use crate::act::{
    Act, Column, ColumnEntry, CopyAct, Edit, Hint, LineAct, ListAct, Move, Panel, Seek,
};
use crate::keys::{Key, KeyPress, Keystroke, Modifiers};
use crate::screens::{Effect, HIDE};
use crate::words::quote;

/// What a key runs: fuxth code, what the column says it does, and the
/// group the column lists it under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub code: String,
    pub label: String,
    pub group: String,
}

impl Binding {
    /// What `bind` makes of `code`, or `bind-root` if `root`: labelled with
    /// its code, in its word's group.
    pub fn new(code: String, root: bool) -> Self {
        Self {
            label: code.clone(),
            code,
            group: Self::group_of(root).to_owned(),
        }
    }

    /// The group `bind` gives, or `bind-root` if `root`.
    fn group_of(root: bool) -> &'static str {
        if root { ROOT_GROUP } else { OTHER_GROUP }
    }

    /// The config line that binds `keys` to it, after the prefix or, if
    /// `root`, without it: its label and group too, unless they are what
    /// that word gives.
    fn line(&self, keys: &str, root: bool) -> String {
        let word = if root { "bind-root" } else { "bind" };
        let mut line = format!("str {} str {} {word}", quote(keys), quote(&self.code));
        if self.label != self.code {
            line += &format!("  str {} label", quote(&self.label));
        }
        if self.group != Self::group_of(root) {
            line += &format!("  str {} group", quote(&self.group));
        }
        line
    }
}

/// What a key of a layer does: runs a binding, or opens a layer of more
/// keys (`t n` is `n` in the layer `t`). Never both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// With `repeat`, its layer stays after it runs: its keys repeat
    /// without the prefix until Esc.
    Run {
        binding: Binding,
        repeat: bool,
    },
    Layer(Layer),
}

/// The keys typed after the prefix, or after a layer's keys, each bound
/// once, in the order first bound. Escape is never one: it closes the
/// column.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layer(Vec<(KeyPress, Node)>);

impl Layer {
    pub fn iter(&self) -> std::slice::Iter<'_, (KeyPress, Node)> {
        self.0.iter()
    }
    pub fn get(&self, key: KeyPress) -> Option<&Node> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, node)| node)
    }
    /// The layer at `path` below this one; this one if `path` is empty.
    pub fn layer(&self, path: &[KeyPress]) -> Option<&Layer> {
        match path.split_first() {
            None => Some(self),
            Some((key, rest)) => match self.get(*key)? {
                Node::Layer(layer) => layer.layer(rest),
                Node::Run { .. } => None,
            },
        }
    }
    /// Every binding in it and its layers, depth first: their keys, and
    /// whether they repeat.
    pub fn all(&self) -> Vec<(Vec<KeyPress>, &Binding, bool)> {
        let mut all = Vec::new();
        for (key, node) in &self.0 {
            let below = match node {
                Node::Run { binding, repeat } => vec![(Vec::new(), binding, *repeat)],
                Node::Layer(layer) => layer.all(),
            };
            all.extend(
                (below.into_iter())
                    .map(|(keys, b, r)| ([*key].into_iter().chain(keys).collect(), b, r)),
            );
        }
        all
    }
    /// What `keys` below this layer are.
    fn node_mut(&mut self, keys: &[KeyPress]) -> Option<&mut Node> {
        let (key, rest) = keys.split_first()?;
        let node = self
            .0
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, node)| node)?;
        match (node, rest.is_empty()) {
            (node, true) => Some(node),
            (Node::Layer(layer), false) => layer.node_mut(rest),
            (Node::Run { .. }, false) => None,
        }
    }

    /// Its first binding, depth first, whose group is its title.
    pub fn first(&self) -> Option<&Binding> {
        self.all().into_iter().next().map(|(_, b, _)| b)
    }

    /// Binds the `rest` of `keys` below this layer to `run`, replacing what
    /// they ran. Keys that are a layer, or that start with keys that run a
    /// binding, are refused.
    fn bind(&mut self, keys: &[KeyPress], rest: &[KeyPress], run: Node) -> Result<(), String> {
        let Some((key, after)) = rest.split_first() else {
            return Err("bind needs keys".to_owned());
        };
        let Some((_, node)) = self.0.iter_mut().find(|(k, _)| k == key) else {
            let mut layer = Layer::default();
            let node = match after.is_empty() {
                true => run,
                false => layer.bind(keys, after, run).map(|()| Node::Layer(layer))?,
            };
            self.0.push((*key, node));
            return Ok(());
        };
        match (node, after.is_empty()) {
            (Node::Layer(layer), false) => layer.bind(keys, after, run),
            (Node::Layer(_), true) => Err(format!(
                "{} is a layer of more keys; unbind it first",
                keys_text(keys)
            )),
            (node @ Node::Run { .. }, true) => {
                *node = run;
                Ok(())
            }
            (Node::Run { binding, .. }, false) => Err(format!(
                "{} runs {}, so it cannot start {}; unbind it first",
                keys_text(keys.strip_suffix(after).unwrap_or(keys)),
                binding.label,
                keys_text(keys)
            )),
        }
    }

    /// Unbinds `keys`: a binding, or a whole layer. Whether they were bound.
    fn unbind(&mut self, keys: &[KeyPress]) -> bool {
        let Some((key, rest)) = keys.split_first() else {
            return false;
        };
        let gone = match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(_) if rest.is_empty() => true,
            Some((_, Node::Layer(layer))) => match layer.unbind(rest) {
                true => layer.0.is_empty(),
                false => return false,
            },
            Some((_, Node::Run { .. })) | None => return false,
        };
        if gone {
            self.0.retain(|(k, _)| k != key);
        }
        true
    }
}

/// Keys as they are written: `t n`.
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

/// Keys from their names, separated by spaces: `t n`.
pub fn keys(words: &str) -> Result<Vec<KeyPress>, String> {
    let keys: Vec<KeyPress> = words
        .split_whitespace()
        .map(key)
        .collect::<Result<_, _>>()?;
    (!keys.is_empty())
        .then_some(keys)
        .ok_or_else(|| "no keys given".to_owned())
}

/// A client's prefix and bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub prefix: KeyPress,
    pub bindings: Layer,
    /// Keys bound without the prefix, in the order bound; never the prefix.
    pub root: Vec<(KeyPress, Binding)>,
    /// The keys of fux's own screens.
    pub modes: Modes,
    /// The config that made it, by its name and text: its definitions are
    /// its bindings' and prompt's words too.
    pub config: Option<std::sync::Arc<(String, String)>>,
}

/// The group of bindings without the prefix that gave none.
const ROOT_GROUP: &str = "Without the prefix";

/// The group of bindings after the prefix that gave none, listed last.
const OTHER_GROUP: &str = "Other";

impl Keymap {
    /// No bindings, and the default prefix, `C-b`: the default keys
    /// (`crate::script::default_keys`) are bound on it.
    pub fn empty() -> Self {
        Self {
            prefix: KeyPress::new(Key::Char('b'), Modifiers::CTRL),
            bindings: Layer::default(),
            root: Vec::new(),
            modes: Modes::default(),
            config: None,
        }
    }

    /// Binds `keys` after the prefix to `binding`.
    pub fn bind(
        &mut self,
        keys: &[KeyPress],
        binding: Binding,
        repeat: bool,
    ) -> Result<(), String> {
        if keys.iter().any(|k| k.plain_key() == Some(Key::Escape)) {
            return Err("Escape after the prefix closes the column; it cannot be bound".to_owned());
        }
        self.bindings
            .bind(keys, keys, Node::Run { binding, repeat })
    }

    /// Binds `key`, without the prefix, to `binding`.
    pub fn bind_root(&mut self, key: KeyPress, binding: Binding) -> Result<(), String> {
        if key == self.prefix {
            return Err(format!(
                "{key} is the prefix; it cannot be bound without it"
            ));
        }
        match self.root.iter_mut().find(|(k, _)| *k == key) {
            Some((_, existing)) => *existing = binding,
            None => self.root.push((key, binding)),
        }
        Ok(())
    }

    /// The binding of `key` without the prefix.
    fn root_binding(&self, key: KeyPress) -> Option<&Binding> {
        self.root.iter().find(|(k, _)| *k == key).map(|(_, b)| b)
    }

    pub fn unbind(&mut self, keys: &[KeyPress]) -> Result<(), String> {
        let unbound = self.bindings.unbind(keys);
        unbound
            .then_some(())
            .ok_or_else(|| format!("{} is not bound", keys_text(keys)))
    }

    pub fn unbind_root(&mut self, key: KeyPress) -> Result<(), String> {
        let before = self.root.len();
        self.root.retain(|(k, _)| *k != key);
        let unbound = self.root.len() != before;
        unbound
            .then_some(())
            .ok_or_else(|| format!("{key} is not bound without the prefix"))
    }

    pub fn unbind_all(&mut self) {
        self.bindings = Layer::default();
        self.root.clear();
        self.modes = Modes::default();
    }

    /// The binding `keys` after the prefix run.
    pub fn binding_mut(&mut self, keys: &[KeyPress]) -> Option<&mut Binding> {
        match self.bindings.node_mut(keys)? {
            Node::Run { binding, .. } => Some(binding),
            Node::Layer(_) => None,
        }
    }

    /// The binding `keys` run repeats: its layer stays until Esc.
    pub fn set_repeat(&mut self, keys: &[KeyPress]) -> Result<(), String> {
        match self.bindings.node_mut(keys) {
            Some(Node::Run { repeat, .. }) => {
                *repeat = true;
                Ok(())
            }
            Some(Node::Layer(_)) | None => Err(format!("{} runs nothing", keys_text(keys))),
        }
    }

    /// Sets the prefix: refused if a key bound without it is the one.
    pub fn set_prefix(&mut self, key: KeyPress) -> Result<(), String> {
        if self.root_binding(key).is_some() {
            return Err(format!(
                "{key} is bound without the prefix; unbind it first"
            ));
        }
        self.prefix = key;
        Ok(())
    }

    /// The column for the layer at `path`, `selected` chosen: its entries
    /// grouped, groups in the order their keys were first bound and `Other`
    /// last; right after the prefix, the bindings without it follow.
    pub fn column(&self, path: &[KeyPress], selected: usize) -> Option<Column> {
        let layer = self.bindings.layer(path)?;
        let mut entries: Vec<ColumnEntry> = (layer.iter())
            .filter_map(|(key, node)| {
                // A layer is titled, and listed, by its first binding's
                // group: `t` is `Tabs…`.
                let (label, group, layer) = match node {
                    Node::Run { binding, .. } => {
                        (binding.label.clone(), binding.group.clone(), false)
                    }
                    Node::Layer(inner) => {
                        let group = inner.first()?.group.clone();
                        (group.clone(), group, true)
                    }
                };
                Some(ColumnEntry {
                    key: *key,
                    label,
                    group,
                    layer,
                    root: false,
                })
            })
            .collect();
        if path.is_empty() {
            entries.extend(self.root.iter().map(|(key, binding)| ColumnEntry {
                key: *key,
                label: binding.label.clone(),
                group: binding.group.clone(),
                layer: false,
                root: true,
            }));
        }
        let mut groups: Vec<(bool, String)> = Vec::new();
        for entry in &entries {
            let group = (entry.root, entry.group.clone());
            if !groups.contains(&group) && group != (false, OTHER_GROUP.to_owned()) {
                groups.push(group);
            }
        }
        groups.push((false, OTHER_GROUP.to_owned()));
        entries.sort_by_key(|e| {
            let group = groups
                .iter()
                .position(|(r, g)| *r == e.root && *g == e.group);
            (e.root, group)
        });
        let keys = keys_text(&[&[self.prefix][..], path].concat());
        let title = match path.is_empty() {
            true => "Commands".to_owned(),
            false => format!("{keys}: {}", layer.first().map_or("", |b| b.group.as_str())),
        };
        let selected = selected.min(entries.len().saturating_sub(1));
        Some(Column {
            title,
            keys,
            entries,
            selected,
        })
    }

    /// A repeat mode's bar for the layer at `path`, if a binding there
    /// repeats: `RESIZE  h j k l · Esc`.
    pub fn repeat_bar(&self, path: &[KeyPress]) -> Option<String> {
        let layer = self.bindings.layer(path)?;
        // The keys that repeat, titled by the first one's group.
        let mut keys = Vec::new();
        let mut title = None;
        for (key, node) in layer.iter() {
            if let Node::Run {
                binding,
                repeat: true,
            } = node
            {
                keys.push(key.to_string());
                title.get_or_insert(binding.group.as_str());
            }
        }
        let title = title?;
        Some(format!(
            "{}  {} · Esc",
            title.to_uppercase(),
            keys.join(" ")
        ))
    }

    /// Every binding, as `list-keys` shows them: config lines that make
    /// them again. Not the config's definitions, which a binding may call.
    pub fn describe(&self) -> String {
        let prefix = quote(&self.prefix.to_string());
        let bound = (self.bindings.all().into_iter()).map(|(keys, binding, repeat)| {
            let repeat = if repeat { "  repeat" } else { "" };
            format!("{}{repeat}\n", binding.line(&keys_text(&keys), false))
        });
        let root = (self.root.iter())
            .map(|(key, binding)| format!("{}\n", binding.line(&key.to_string(), true)));
        std::iter::once(format!("unbind-all\nstr {prefix} prefix\n"))
            .chain(bound)
            .chain(root)
            .chain([self.modes.lines()])
            .collect()
    }
}

/// What the client's keys are in the middle of, of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Held {
    #[default]
    Normal,
    /// The column is shown, on the layer at `path`.
    Column {
        path: Vec<KeyPress>,
        selected: usize,
    },
    /// A repeat mode, of the layer at `path`.
    Repeat { path: Vec<KeyPress> },
    /// A client that only watches typed its prefix.
    Prefixed,
}

impl Held {
    /// A key typed at the client's terminal while none of its screens is
    /// open (`crate::screens`): what it leads to. `read_only`: it only
    /// watches, and the prefix then `d` detaches it.
    pub fn press(&mut self, keymap: &Keymap, stroke: Keystroke, read_only: bool) -> Vec<Effect> {
        let press = stroke.press;
        if read_only {
            return self.watching(keymap, press);
        }
        match std::mem::take(self) {
            Held::Normal | Held::Prefixed => {
                if press == keymap.prefix {
                    return self.open(keymap, Vec::new(), 0);
                }
                match keymap.root_binding(press) {
                    Some(binding) => vec![Effect::Run(binding.code.as_str().into())],
                    None => vec![Effect::Send(Act::Key(stroke))],
                }
            }
            Held::Column { path, selected } => self.in_column(keymap, path, selected, stroke),
            Held::Repeat { path } => self.repeating(keymap, path, press),
        }
    }

    /// The column for the layer at `path`, shown.
    fn open(&mut self, keymap: &Keymap, path: Vec<KeyPress>, selected: usize) -> Vec<Effect> {
        match keymap.column(&path, selected) {
            Some(column) => {
                let selected = column.selected;
                *self = Held::Column { path, selected };
                vec![Effect::Screen(ScreenAct::Show(Panel::Column(column)))]
            }
            None => vec![HIDE],
        }
    }

    /// The keys in `path` and `press` typed: its binding runs, entering its
    /// layer's repeat mode if it repeats; or its layer opens; or, unbound,
    /// the column stays, saying so.
    fn follow(&mut self, keymap: &Keymap, path: Vec<KeyPress>, press: KeyPress) -> Vec<Effect> {
        let layer = keymap.bindings.layer(&path);
        match layer.and_then(|l| l.get(press)) {
            Some(Node::Run { binding, repeat }) => {
                let bar = repeat.then(|| keymap.repeat_bar(&path)).flatten();
                let shown = match bar {
                    Some(bar) => {
                        *self = Held::Repeat { path };
                        ScreenAct::Show(Panel::Repeat(bar))
                    }
                    None => {
                        *self = Held::Normal;
                        ScreenAct::Hide
                    }
                };
                vec![
                    Effect::Screen(shown),
                    Effect::Run(binding.code.as_str().into()),
                ]
            }
            Some(Node::Layer(_)) => {
                let mut deeper = path;
                deeper.push(press);
                self.open(keymap, deeper, 0)
            }
            None => {
                let mut keys = path.clone();
                keys.push(press);
                *self = Held::Column { path, selected: 0 };
                let text = format!("{} {} is not bound", keymap.prefix, keys_text(&keys));
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
        if keymap
            .bindings
            .layer(&path)
            .is_some_and(|l| l.get(press).is_some())
        {
            return self.follow(keymap, path, press);
        }
        if press == keymap.prefix {
            // The prefix, at any depth, goes to the pane.
            return vec![HIDE, Effect::Send(Act::Key(stroke))];
        }
        let count = keymap.column(&path, 0).map_or(0, |c| c.entries.len());
        let last = count.saturating_sub(1);
        // A list's moves, but by its keys that are not letters, which are
        // bindings here.
        let unbound = press.mods == Modifiers::NONE && !matches!(press.key, Key::Char(_));
        if let Some(ListAct::Move(moved)) = keymap.modes.list.get(press).filter(|_| unbound) {
            return self.open(keymap, path, moved.from(selected, last));
        }
        if press.key == Key::Enter && press.mods == Modifiers::NONE {
            let chosen = keymap.column(&path, selected).and_then(|c| {
                let entry = c.entries.into_iter().nth(c.selected)?;
                Some((entry.key, entry.root))
            });
            return match chosen {
                // A key bound without the prefix, listed after the rest.
                Some((press, true)) => {
                    let code = keymap.root_binding(press).map(|b| b.code.clone());
                    std::iter::once(HIDE)
                        .chain(code.map(|c| Effect::Run(c.into())))
                        .collect()
                }
                Some((key, false)) => self.follow(keymap, path, key),
                None => vec![HIDE],
            };
        }
        self.follow(keymap, path, press)
    }

    fn repeating(&mut self, keymap: &Keymap, path: Vec<KeyPress>, press: KeyPress) -> Vec<Effect> {
        if press == keymap.prefix {
            return self.open(keymap, Vec::new(), 0);
        }
        let bound = keymap.bindings.layer(&path).and_then(|l| l.get(press));
        let plain = !press.mods.ctrl && !press.mods.alt;
        let leaves = press.key == Key::Escape || (press.key == Key::Enter && bound.is_none());
        if plain && leaves {
            return vec![HIDE];
        }
        match bound {
            Some(Node::Run {
                binding,
                repeat: true,
            }) => {
                let code = binding.code.clone();
                *self = Held::Repeat { path };
                vec![Effect::Run(code.into())]
            }
            // A key of the layer that does not repeat runs once, and ends
            // the mode.
            Some(Node::Run {
                binding,
                repeat: false,
            }) => {
                vec![HIDE, Effect::Run(binding.code.as_str().into())]
            }
            Some(Node::Layer(_)) | None => {
                let text = format!("{press} is not one of this mode's keys");
                vec![HIDE, Effect::Send(Act::Notice { text, error: false })]
            }
        }
    }

    fn watching(&mut self, keymap: &Keymap, press: KeyPress) -> Vec<Effect> {
        let prefixed = std::mem::take(self) == Held::Prefixed;
        if prefixed && press == KeyPress::char('d') {
            return vec![Effect::Run("me detach".into())];
        }
        if !prefixed && press == keymap.prefix {
            *self = Held::Prefixed;
            return Vec::new();
        }
        vec![Effect::Send(watching(keymap))]
    }
}

/// What a client that only watches is told of a key or paste it takes no
/// further: its own way out.
pub fn watching(keymap: &Keymap) -> Act {
    let text = format!("watching only: {} d detaches", keymap.prefix);
    Act::Notice { text, error: false }
}

/// The keys of fux's own screens, from the config as the bindings are
/// (`bind-in`): each a key to one of its screen's actions, and the hint
/// that names it there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub copy: Table<CopyAct>,
    pub list: Table<ListAct>,
    pub line: Table<LineAct>,
    pub confirm: Table<bool>,
}

/// One of fux's own screens, as `bind-in` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    Copy,
    List,
    Line,
    Confirm,
}

impl Modal {
    pub const ALL: &[(&str, Modal)] = &[
        ("copy", Modal::Copy),
        ("list", Modal::List),
        ("line", Modal::Line),
        ("confirm", Modal::Confirm),
    ];
}

impl Modes {
    pub fn table(&mut self, modal: Modal) -> &mut dyn Bindable {
        match modal {
            Modal::Copy => &mut self.copy,
            Modal::List => &mut self.list,
            Modal::Line => &mut self.line,
            Modal::Confirm => &mut self.confirm,
        }
    }

    /// Every key, as config lines that bind it again.
    fn lines(&self) -> String {
        let tables: [&dyn Bindable; 4] = [&self.copy, &self.list, &self.line, &self.confirm];
        (Modal::ALL.iter().zip(tables))
            .map(|((name, _), table)| table.lines(name))
            .collect()
    }
}

/// The actions of one of fux's screens, by the names `bind-in` reads; and
/// what Escape does there, always, so that no screen holds a key it cannot
/// leave.
pub trait Named: Clone + PartialEq + 'static {
    const NAMES: &'static [(&'static str, Self)];
    const ESCAPE: Self;

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

/// A screen's table, whatever its actions: what a config's words change.
pub trait Bindable {
    /// Binds `key` to the action named `name`, in place of what it did.
    fn bind(&mut self, key: KeyPress, name: &str) -> Result<(), String>;
    fn unbind(&mut self, key: KeyPress) -> Result<(), String>;
    /// The hint of `key`'s action, if it is bound.
    fn label(&mut self, key: KeyPress) -> Option<&mut Option<String>>;
    /// Its keys as config lines, for the screen named `modal`.
    fn lines(&self, modal: &str) -> String;
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
                crate::words::listed(&names, "or")
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
            format!("str {modal} str {key} str {} bind-in{label}\n", act.name())
        };
        self.0.iter().map(line).collect()
    }
}

/// A key in copy mode: an action for the server; the search line opening
/// (`CopyAct::Typing`) the client's own too.
impl Named for CopyAct {
    #[rustfmt::skip]
    const NAMES: &'static [(&'static str, Self)] = &[
        ("left", CopyAct::Left), ("right", CopyAct::Right), ("up", CopyAct::Up),
        ("down", CopyAct::Down), ("word-forward", CopyAct::WordForward),
        ("word-back", CopyAct::WordBack), ("first-ink", CopyAct::FirstInk),
        ("last-ink", CopyAct::LastInk), ("line-start", CopyAct::LineStart),
        ("top", CopyAct::Top), ("bottom", CopyAct::Bottom), ("half-up", CopyAct::HalfUp),
        ("half-down", CopyAct::HalfDown), ("page-up", CopyAct::PageUp),
        ("page-down", CopyAct::PageDown), ("next", CopyAct::Next),
        ("previous", CopyAct::Previous), ("prompt-back", CopyAct::Prompt(Seek::Backward)),
        ("prompt-forward", CopyAct::Prompt(Seek::Forward)), ("select-char", CopyAct::SelectChar),
        ("select-line", CopyAct::SelectLine), ("select-block", CopyAct::SelectBlock),
        ("other-end", CopyAct::OtherEnd), ("yank", CopyAct::Yank), ("leave", CopyAct::Leave),
        ("search-forward", CopyAct::Typing(Some((Seek::Forward, String::new(), String::new())))),
        ("search-back", CopyAct::Typing(Some((Seek::Backward, String::new(), String::new())))),
    ];
    const ESCAPE: Self = CopyAct::Leave;
}

impl Named for ListAct {
    #[rustfmt::skip]
    const NAMES: &'static [(&'static str, Self)] = &[
        ("up", ListAct::Move(Move::Up)), ("down", ListAct::Move(Move::Down)),
        ("page-up", ListAct::Move(Move::PageUp)), ("page-down", ListAct::Move(Move::PageDown)),
        ("first", ListAct::Move(Move::First)), ("last", ListAct::Move(Move::Last)),
        ("choose", ListAct::Choose), ("cancel", ListAct::Cancel),
        ("rename", ListAct::Rename), ("close", ListAct::Close),
    ];
    const ESCAPE: Self = ListAct::Cancel;
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

/// A question's answer: yes or no.
impl Named for bool {
    const NAMES: &'static [(&'static str, Self)] = &[("yes", true), ("no", false)];
    const ESCAPE: Self = false;
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
    use super::*;
    use crate::act::PAGE;

    fn defaults() -> Result<Keymap, String> {
        crate::script::default_keys().map_err(|e| e.to_string())
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
        check::<CopyAct>("top", "bottom")?;
        check::<ListAct>("up", "down")?;
        check::<LineAct>("home", "end")?;
        check::<bool>("no", "yes")
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
        let mut groups: Vec<String> = (keymap.column(&[], 0).ok_or("a column")?.entries)
            .into_iter()
            .map(|e| e.group)
            .collect();
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
            keymap.repeat_bar(&keys("r")?).as_deref(),
            Some("RESIZE  h j k l · Esc")
        );
        Ok(())
    }

    #[test]
    fn the_prefix_opens_the_column_a_key_runs_its_binding() -> Result<(), String> {
        let keymap = defaults()?;
        let mut held = Held::default();
        let steps = held.press(&keymap, stroke("C-b")?, false);
        assert!(matches!(
            steps.as_slice(),
            [Effect::Screen(ScreenAct::Show(Panel::Column(_)))]
        ));
        let steps = held.press(&keymap, stroke("z")?, false);
        assert_eq!(
            steps,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run("me zoom".into())
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

    #[test]
    fn a_repeat_mode_keeps_its_layer_until_escape() -> Result<(), String> {
        let keymap = defaults()?;
        let mut held = Held::default();
        for name in ["C-b", "r", "h"] {
            held.press(&keymap, stroke(name)?, false);
        }
        assert!(matches!(held, Held::Repeat { .. }));
        let steps = held.press(&keymap, stroke("l")?, false);
        assert_eq!(
            steps,
            [Effect::Run("me focused str right 1 move-border".into())]
        );
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
        assert_eq!(steps, [Effect::Run("me detach".into())]);
        Ok(())
    }

    /// The column's own keys move its selection within its bindings; a
    /// letter is a binding's, and Enter runs the selected one, the keys
    /// bound without the prefix listed last.
    #[test]
    fn the_column_scrolls_within_its_bindings() -> Result<(), String> {
        let mut keymap = defaults()?;
        keymap.bind_root(key("M-t")?, Binding::new("opts new-tab drop".into(), true))?;
        let entries = keymap.column(&[], 0).map(|c| c.entries).unwrap_or_default();
        let last = entries.len().saturating_sub(1);
        assert!(
            entries
                .last()
                .is_some_and(|e| e.root && e.group == ROOT_GROUP),
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
            [Effect::Screen(ScreenAct::Show(Panel::Column(column)))] => Some(column.selected),
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
                Effect::Run("opts new-tab drop".into())
            ]
        );
        press("C-b")?;
        assert_eq!(
            press("j")?,
            [
                Effect::Screen(ScreenAct::Hide),
                Effect::Run("me str down focus".into())
            ]
        );
        Ok(())
    }

    #[test]
    fn list_keys_shows_sequences_repeats_and_keys_without_the_prefix() -> Result<(), String> {
        let mut keymap = Keymap::empty();
        keymap.bind(
            &keys("g n")?,
            Binding::new("opts new-tab drop".into(), false),
            false,
        )?;
        keymap.bind(
            &keys("y l")?,
            Binding::new("me focused str right 1 move-border".into(), false),
            true,
        )?;
        keymap.bind_root(key("M-t")?, Binding::new("me zoom".into(), true))?;
        assert_eq!(
            keymap.describe(),
            "unbind-all\nstr C-b prefix\nstr 'g n' str 'opts new-tab drop' bind\nstr 'y l' str 'me focused str right 1 move-border' bind  repeat\nstr M-t str 'me zoom' bind-root\n"
        );
        Ok(())
    }
}
