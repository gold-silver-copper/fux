//! A client's keys: its prefix and bindings, the command column and repeat
//! modes it shows, and the tables that turn its keys into the actions of
//! fux's own screens. All of it in the client: the server is sent typed
//! actions, and keys only as literal input to a pane.
//!
//! A binding runs fuxth: its code is compiled and run by the client, fresh,
//! each time its keys are typed, the client itself its `me`.
use crate::act::{Act, Column, ColumnEntry, CopyAct, Edit, LineAct, ListAct, Panel};
use crate::keys::{Direction, Key, KeyPress, Keystroke, Modifiers};
use crate::screens::Effect;

/// What a key runs: fuxth code, what the column says it does, and the
/// group the column lists it under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub code: String,
    pub label: String,
    pub group: String,
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
    let press: KeyPress = word
        .parse()
        .map_err(|_| format!("{word:?} is no key's name"))?;
    // A character's shift is dropped in parsing it: `S-` is read again.
    let mut rest = word;
    let mut shift = false;
    while let Some((modifier, tail)) = rest.split_at_checked(2) {
        if tail.is_empty() {
            break;
        }
        match modifier {
            "S-" | "s-" => shift = true,
            "C-" | "c-" | "M-" | "m-" => {}
            _ => break,
        }
        rest = tail;
    }
    if let Key::Char(c) = press.key
        && shift
        && c.is_ascii_lowercase()
    {
        return Ok(KeyPress::new(Key::Char(c.to_ascii_uppercase()), press.mods));
    }
    Ok(press)
}

/// Keys from their names, separated by spaces: `t n`.
pub fn keys(words: &str) -> Result<Vec<KeyPress>, String> {
    let keys: Vec<KeyPress> = words
        .split_whitespace()
        .map(key)
        .collect::<Result<_, _>>()?;
    match keys.is_empty() {
        true => Err("no keys given".to_owned()),
        false => Ok(keys),
    }
}

fn is_escape(press: &KeyPress) -> bool {
    press.key == Key::Escape && !press.mods.ctrl && !press.mods.alt
}

/// A client's prefix and bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub prefix: KeyPress,
    pub bindings: Layer,
    /// Keys bound without the prefix, in the order bound; never the prefix.
    pub root: Vec<(KeyPress, Binding)>,
}

/// The groups of the column, in its order; custom groups follow, `Other`
/// last.
const GROUPS: [&str; 5] = ["Panes", "Focus", "Tabs", "Workspaces", "Session"];

/// The group of bindings without the prefix that gave none.
pub const ROOT_GROUP: &str = "Without the prefix";

/// A group's bindings: keys after the prefix, fuxth and label.
type Bindings = &'static [(&'static str, &'static str, &'static str)];

/// The default bindings, by group, and whether the group's layer
/// repeats. After the prefix every default key is a lower-case
/// letter: `r` and `m` are repeat modes, `t` and `w` layers sharing their
/// verbs.
pub const DEFAULTS: &[(&str, bool, Bindings)] = &[
    (
        "Focus",
        false,
        &[
            ("h", "me str left focus", "focus left"),
            ("j", "me str down focus", "focus down"),
            ("k", "me str up focus", "focus up"),
            ("l", "me str right focus", "focus right"),
            ("o", "me str next focus", "next pane"),
            ("q", "me str last focus", "last pane"),
        ],
    ),
    (
        "Panes",
        false,
        &[
            (
                "v",
                "me focused opts horizontal split drop",
                "split side by side",
            ),
            ("s", "me focused opts vertical split drop", "split stacked"),
            ("x", "me me focused confirm-close", "close pane"),
            ("z", "me zoom", "zoom or restore"),
            ("a", "me me focused menu", "pane actions"),
            ("c", "me copy-mode", "copy and select"),
            ("p", "me focused 0 paste-buffer", "paste the newest copy"),
        ],
    ),
    (
        "Resize",
        true,
        &[
            (
                "r h",
                "me focused str left 1 move-border",
                "shrink or grow left",
            ),
            (
                "r j",
                "me focused str down 1 move-border",
                "shrink or grow down",
            ),
            (
                "r k",
                "me focused str up 1 move-border",
                "shrink or grow up",
            ),
            (
                "r l",
                "me focused str right 1 move-border",
                "shrink or grow right",
            ),
        ],
    ),
    (
        "Move",
        true,
        &[
            ("m h", "me focused str left move-beside", "move left"),
            ("m j", "me focused str down move-beside", "move down"),
            ("m k", "me focused str up move-beside", "move up"),
            ("m l", "me focused str right move-beside", "move right"),
        ],
    ),
    (
        "Tabs",
        false,
        &[
            ("n", "me next-tab", "next tab"),
            ("b", "me previous-tab", "previous tab"),
            ("t n", "opts new-tab drop", "new tab"),
            ("t h", "me previous-tab", "previous tab"),
            ("t l", "me next-tab", "next tab"),
            ("t g", "me choose-tab", "choose a tab"),
            ("t r", "me me focused tab rename-prompt", "rename tab"),
            ("t x", "me me focused tab confirm-close", "close tab"),
            ("t a", "me me focused tab menu", "tab actions"),
        ],
    ),
    (
        "Reorder",
        true,
        &[
            (
                "t m h",
                "me focused tab str previous reorder",
                "move tab earlier",
            ),
            ("t m l", "me focused tab str next reorder", "move tab later"),
        ],
    ),
    (
        "Workspaces",
        false,
        &[
            ("w n", "opts new-workspace drop", "new workspace"),
            ("w h", "me previous-workspace", "previous workspace"),
            ("w l", "me next-workspace", "next workspace"),
            ("w g", "me choose-workspace", "choose a workspace"),
            (
                "w r",
                "me me focused workspace rename-prompt",
                "rename workspace",
            ),
            (
                "w x",
                "me me focused workspace confirm-close",
                "close workspace",
            ),
            ("w a", "me me focused workspace menu", "workspace actions"),
        ],
    ),
    (
        "Reorder",
        true,
        &[
            (
                "w m h",
                "me focused workspace str previous reorder",
                "move workspace earlier",
            ),
            (
                "w m l",
                "me focused workspace str next reorder",
                "move workspace later",
            ),
        ],
    ),
    (
        "Session",
        false,
        &[
            ("e", "me command-prompt", "command prompt"),
            ("d", "me detach", "detach"),
        ],
    ),
];

impl Default for Keymap {
    fn default() -> Keymap {
        let mut keymap = Keymap {
            prefix: KeyPress::new(Key::Char('b'), Modifiers::CTRL),
            bindings: Layer::default(),
            root: Vec::new(),
        };
        for (group, repeat, bindings) in DEFAULTS {
            for (keys, code, label) in *bindings {
                let binding = Binding {
                    code: (*code).to_owned(),
                    label: (*label).to_owned(),
                    group: (*group).to_owned(),
                };
                let _ = self::keys(keys).and_then(|keys| keymap.bind(&keys, binding, *repeat));
            }
        }
        keymap
    }
}

impl Keymap {
    /// Binds `keys` after the prefix to `binding`.
    pub fn bind(
        &mut self,
        keys: &[KeyPress],
        binding: Binding,
        repeat: bool,
    ) -> Result<(), String> {
        if keys.iter().any(is_escape) {
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
        match self.bindings.unbind(keys) {
            true => Ok(()),
            false => Err(format!("{} is not bound", keys_text(keys))),
        }
    }

    pub fn unbind_root(&mut self, key: KeyPress) -> Result<(), String> {
        let before = self.root.len();
        self.root.retain(|(k, _)| *k != key);
        match self.root.len() == before {
            true => Err(format!("{key} is not bound without the prefix")),
            false => Ok(()),
        }
    }

    pub fn unbind_all(&mut self) {
        self.bindings = Layer::default();
        self.root.clear();
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
    /// grouped, groups in their order, custom groups after them and `Other`
    /// last; right after the prefix, the bindings without it follow.
    pub fn column(&self, path: &[KeyPress], selected: usize) -> Option<Column> {
        let layer = self.bindings.layer(path)?;
        let mut entries: Vec<(bool, ColumnEntry)> = (layer.iter())
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
                Some((
                    false,
                    ColumnEntry {
                        key: key.to_string(),
                        label,
                        group,
                        layer,
                        root: false,
                    },
                ))
            })
            .collect();
        if path.is_empty() {
            entries.extend(self.root.iter().map(|(key, binding)| {
                let group = match binding.group.as_str() {
                    "" => ROOT_GROUP.to_owned(),
                    group => group.to_owned(),
                };
                (
                    true,
                    ColumnEntry {
                        key: key.to_string(),
                        label: binding.label.clone(),
                        group,
                        layer: false,
                        root: true,
                    },
                )
            }));
        }
        let mut groups: Vec<(bool, String)> = GROUPS.map(|g| (false, g.to_owned())).to_vec();
        for (root, entry) in &entries {
            let group = (*root, entry.group.clone());
            if !groups.contains(&group) && group != (false, "Other".to_owned()) {
                groups.push(group);
            }
        }
        groups.push((false, "Other".to_owned()));
        entries.sort_by_key(|(root, e)| {
            let group = groups.iter().position(|(r, g)| r == root && *g == e.group);
            (*root, group)
        });
        let title = match path.is_empty() {
            true => "Commands".to_owned(),
            false => format!(
                "{} {}: {}",
                self.prefix,
                keys_text(path),
                layer.first().map_or("", |b| b.group.as_str())
            ),
        };
        let entries: Vec<ColumnEntry> = entries.into_iter().map(|(_, e)| e).collect();
        let selected = selected.min(entries.len().saturating_sub(1));
        let keys =
            std::iter::once(self.prefix.to_string()).chain(path.iter().map(KeyPress::to_string));
        Some(Column {
            title,
            keys: keys.collect::<Vec<_>>().join(" "),
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

    /// Every binding, as `list-keys` shows them.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "After the prefix, {} (case counts, V is Shift-v):\n",
            self.prefix
        );
        for (keys, binding, repeat) in self.bindings.all() {
            out.push_str(&format!(
                "{:>8}  {}{}\n",
                keys_text(&keys),
                binding.code,
                if repeat { "  (repeats)" } else { "" }
            ));
        }
        if !self.root.is_empty() {
            out.push_str("\nWithout the prefix:\n");
            for (key, binding) in &self.root {
                out.push_str(&format!("{:>8}  {}\n", key.to_string(), binding.code));
            }
        }
        out
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

/// The rows the column, and a screen's list, page by.
pub const PAGE: usize = 10;

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
                    Some(binding) => vec![Effect::Run(binding.code.clone())],
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
                vec![Effect::Send(Act::Show(Panel::Column(column)))]
            }
            None => vec![Effect::Send(Act::Hide)],
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
                        Act::Show(Panel::Repeat(bar))
                    }
                    None => {
                        *self = Held::Normal;
                        Act::Hide
                    }
                };
                vec![Effect::Send(shown), Effect::Run(binding.code.clone())]
            }
            Some(Node::Layer(_)) => {
                let mut deeper = path;
                deeper.push(press);
                self.open(keymap, deeper, 0)
            }
            None => {
                let mut keys = path.clone();
                keys.push(press);
                let selected = 0;
                *self = Held::Column { path, selected };
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
        if is_escape(&press) {
            return vec![Effect::Send(Act::Hide)];
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
            return vec![Effect::Send(Act::Hide), Effect::Send(Act::Key(stroke))];
        }
        let count = keymap.column(&path, 0).map_or(0, |c| c.entries.len());
        let last = count.saturating_sub(1);
        let moved = match (press.mods == Modifiers::NONE).then_some(press.key) {
            Some(Key::Arrow(Direction::Up)) => Some(selected.saturating_sub(1)),
            Some(Key::Arrow(Direction::Down)) => Some(selected.saturating_add(1).min(last)),
            Some(Key::PageUp) => Some(selected.saturating_sub(PAGE)),
            Some(Key::PageDown) => Some(selected.saturating_add(PAGE).min(last)),
            Some(Key::Home) => Some(0),
            Some(Key::End) => Some(last),
            _ => None,
        };
        if let Some(selected) = moved {
            return self.open(keymap, path, selected);
        }
        if press.key == Key::Enter && press.mods == Modifiers::NONE {
            let chosen = keymap.column(&path, selected).and_then(|c| {
                let entry = c.entries.into_iter().nth(c.selected)?;
                Some((key(&entry.key).ok()?, entry.root))
            });
            return match chosen {
                // A key bound without the prefix, listed after the rest.
                Some((press, true)) => {
                    let code = keymap.root_binding(press).map(|b| b.code.clone());
                    std::iter::once(Effect::Send(Act::Hide))
                        .chain(code.map(Effect::Run))
                        .collect()
                }
                Some((key, false)) => self.follow(keymap, path, key),
                None => vec![Effect::Send(Act::Hide)],
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
            return vec![Effect::Send(Act::Hide)];
        }
        match bound {
            Some(Node::Run {
                binding,
                repeat: true,
            }) => {
                let code = binding.code.clone();
                *self = Held::Repeat { path };
                vec![Effect::Run(code)]
            }
            // A key of the layer that does not repeat runs once, and ends
            // the mode.
            Some(Node::Run {
                binding,
                repeat: false,
            }) => {
                vec![Effect::Send(Act::Hide), Effect::Run(binding.code.clone())]
            }
            Some(Node::Layer(_)) | None => {
                let text = format!("{press} is not one of this mode's keys");
                vec![
                    Effect::Send(Act::Hide),
                    Effect::Send(Act::Notice { text, error: false }),
                ]
            }
        }
    }

    fn watching(&mut self, keymap: &Keymap, press: KeyPress) -> Vec<Effect> {
        if std::mem::take(self) == Held::Prefixed {
            return match press == KeyPress::char('d') {
                true => vec![Effect::Run("me detach".to_owned())],
                false => Vec::new(),
            };
        }
        if press == keymap.prefix {
            *self = Held::Prefixed;
            return Vec::new();
        }
        let text = format!("watching only: {} d detaches", keymap.prefix);
        vec![Effect::Send(Act::Notice { text, error: false })]
    }
}

/// A key in a list: moving, choosing, leaving; `r` renames and `x` closes
/// what the chosen entry is about.
pub fn list_act(press: KeyPress) -> Option<ListAct> {
    match press.plain_key()? {
        Key::Arrow(Direction::Up) | Key::Char('k') => Some(ListAct::Up),
        Key::Arrow(Direction::Down) | Key::Char('j') => Some(ListAct::Down),
        Key::PageUp => Some(ListAct::PageUp),
        Key::PageDown => Some(ListAct::PageDown),
        Key::Home => Some(ListAct::First),
        Key::End => Some(ListAct::Last),
        Key::Enter => Some(ListAct::Choose),
        Key::Escape | Key::Char('q') => Some(ListAct::Cancel),
        Key::Char('r') => Some(ListAct::Rename),
        Key::Char('x') => Some(ListAct::Close),
        Key::Char(_)
        | Key::Tab
        | Key::Backspace
        | Key::Insert
        | Key::Delete
        | Key::Arrow(Direction::Left | Direction::Right)
        | Key::F(_) => None,
    }
}

/// A key in a line being typed: a one-line editor.
pub fn line_act(press: KeyPress) -> Option<LineAct> {
    let edit = |edit| Some(LineAct::Edit(edit));
    match press.key {
        Key::Escape => Some(LineAct::Cancel),
        Key::Enter => Some(LineAct::Submit),
        Key::Backspace => edit(Edit::Backspace),
        Key::Delete => edit(Edit::Delete),
        Key::Arrow(Direction::Left) => edit(Edit::Left),
        Key::Arrow(Direction::Right) => edit(Edit::Right),
        Key::Home => edit(Edit::Home),
        Key::End => edit(Edit::End),
        // Text: a letter with Ctrl or Alt types nothing.
        Key::Char(c) if !press.mods.ctrl && !press.mods.alt && !c.is_control() => {
            edit(Edit::Insert(c.to_string()))
        }
        Key::Char(_)
        | Key::Tab
        | Key::Insert
        | Key::Arrow(_)
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => None,
    }
}

/// The letter a key types with neither Ctrl nor Alt, in either case, as
/// lower case.
pub fn letter(press: KeyPress) -> Option<char> {
    let Some(Key::Char(c)) = press.plain_key() else {
        return None;
    };
    Some(c.to_ascii_lowercase())
}

/// A key while a confirmation waits: `y` yes; `n`, `q` or Escape no.
pub fn confirm_act(press: KeyPress) -> Option<bool> {
    match press.plain_key()? {
        Key::Char('y') => Some(true),
        Key::Char('n' | 'q') | Key::Escape => Some(false),
        Key::Char(_)
        | Key::Enter
        | Key::Tab
        | Key::Backspace
        | Key::Insert
        | Key::Delete
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::Arrow(_)
        | Key::F(_) => None,
    }
}

/// A key in copy mode that moves, selects, copies or leaves. Letters, in
/// either case, and the brackets; one with Ctrl or Alt is no key's. The
/// arrows, paging keys, Home, End, Enter and Esc too. `f` and `r`, which
/// open its search line, are the client's own (`crate::screens`).
pub fn copy_act(press: KeyPress) -> Option<CopyAct> {
    let act = match (letter(press), press.key) {
        (Some('q'), _) | (_, Key::Escape) => CopyAct::Leave,
        (Some('h'), _) | (_, Key::Arrow(Direction::Left)) => CopyAct::Left,
        (Some('l'), _) | (_, Key::Arrow(Direction::Right)) => CopyAct::Right,
        (Some('j'), _) | (_, Key::Arrow(Direction::Down)) => CopyAct::Down,
        (Some('k'), _) | (_, Key::Arrow(Direction::Up)) => CopyAct::Up,
        (Some('w'), _) => CopyAct::WordForward,
        (Some('b'), _) => CopyAct::WordBack,
        (Some('a'), _) => CopyAct::FirstInk,
        (Some('e'), _) | (_, Key::End) => CopyAct::LastInk,
        (_, Key::Home) => CopyAct::LineStart,
        (Some('t'), _) => CopyAct::Top,
        (Some('z'), _) => CopyAct::Bottom,
        (Some('u'), _) => CopyAct::HalfUp,
        (Some('d'), _) => CopyAct::HalfDown,
        (_, Key::PageUp) => CopyAct::PageUp,
        (_, Key::PageDown) => CopyAct::PageDown,
        (Some('n'), _) => CopyAct::Next,
        (Some('p'), _) => CopyAct::Previous,
        (Some('['), _) => CopyAct::PromptBack,
        (Some(']'), _) => CopyAct::PromptForward,
        (Some('v'), _) => CopyAct::SelectChar,
        (Some('s'), _) => CopyAct::SelectLine,
        (Some('x'), _) => CopyAct::SelectBlock,
        (Some('o'), _) => CopyAct::OtherEnd,
        (Some('y'), _) | (_, Key::Enter) => CopyAct::Yank,
        _ => return None,
    };
    Some(act)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(name: &str) -> Result<Keystroke, String> {
        key(name).map(Keystroke::from)
    }

    #[test]
    fn every_default_is_bound() {
        let keymap = Keymap::default();
        let defaults: usize = DEFAULTS.iter().map(|(_, _, bindings)| bindings.len()).sum();
        assert_eq!(keymap.bindings.all().len(), defaults);
        assert!(keymap.root.is_empty());
    }

    #[test]
    fn a_shifted_letter_is_its_upper_case() {
        assert_eq!(key("S-v").map(|k| k.key), Ok(Key::Char('V')));
        assert_eq!(key("s-M-v").map(|k| k.key), Ok(Key::Char('V')));
        assert_eq!(key("S-1").map(|k| k.key), Ok(Key::Char('1')));
    }

    #[test]
    fn the_defaults_bind_and_list_in_groups() {
        let keymap = Keymap::default();
        let column = keymap.column(&[], 0);
        let groups: Vec<String> = column
            .map(|c| c.entries.into_iter().map(|e| e.group).collect())
            .unwrap_or_default();
        let first = groups.iter().position(|g| g == "Panes");
        let focus = groups.iter().position(|g| g == "Focus");
        assert!(first < focus, "{groups:?}");
        assert_eq!(
            keymap.repeat_bar(&keys("r").unwrap_or_default()).as_deref(),
            Some("RESIZE  h j k l · Esc")
        );
    }

    #[test]
    fn the_prefix_opens_the_column_a_key_runs_its_binding() -> Result<(), String> {
        let keymap = Keymap::default();
        let mut held = Held::default();
        let steps = held.press(&keymap, stroke("C-b")?, false);
        assert!(matches!(
            steps.as_slice(),
            [Effect::Send(Act::Show(Panel::Column(_)))]
        ));
        let steps = held.press(&keymap, stroke("z")?, false);
        assert_eq!(
            steps,
            [Effect::Send(Act::Hide), Effect::Run("me zoom".into())]
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
        let keymap = Keymap::default();
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
        let keymap = Keymap::default();
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
        let mut keymap = Keymap::default();
        let binding = |code: &str| Binding {
            code: code.into(),
            label: code.into(),
            group: String::new(),
        };
        keymap.bind_root(key("M-t")?, binding("opts new-tab drop"))?;
        let entries = keymap.column(&[], 0).map(|c| c.entries).unwrap_or_default();
        let last = entries.len().saturating_sub(1);
        assert!(
            entries
                .last()
                .is_some_and(|e| e.root && e.group == ROOT_GROUP),
            "{entries:?}"
        );
        assert_eq!(
            entries.first().map(|e| e.key.as_str()),
            Some("v"),
            "Panes come first"
        );
        let mut held = Held::default();
        let mut press = |name: &str| -> Result<Vec<Effect>, String> {
            Ok(held.press(&keymap, stroke(name)?, false))
        };
        press("C-b")?;
        let selected = |steps: Vec<Effect>| match steps.as_slice() {
            [Effect::Send(Act::Show(Panel::Column(column)))] => Some(column.selected),
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
                Effect::Send(Act::Hide),
                Effect::Run("opts new-tab drop".into())
            ]
        );
        press("C-b")?;
        assert_eq!(
            press("j")?,
            [
                Effect::Send(Act::Hide),
                Effect::Run("me str down focus".into())
            ]
        );
        Ok(())
    }

    #[test]
    fn list_keys_shows_sequences_repeats_and_keys_without_the_prefix() -> Result<(), String> {
        let mut keymap = Keymap::default();
        keymap.unbind_all();
        let binding = |code: &str| Binding {
            code: code.into(),
            label: code.into(),
            group: "Other".into(),
        };
        keymap.bind(&keys("g n")?, binding("opts new-tab drop"), false)?;
        keymap.bind(
            &keys("y l")?,
            binding("me focused str right 1 move-border"),
            true,
        )?;
        keymap.bind_root(key("M-t")?, binding("me zoom"))?;
        assert_eq!(
            keymap.describe(),
            "After the prefix, C-b (case counts, V is Shift-v):\n     g n  opts new-tab drop\n     y l  me focused str right 1 move-border  (repeats)\n\nWithout the prefix:\n     M-t  me zoom\n"
        );
        Ok(())
    }
}
