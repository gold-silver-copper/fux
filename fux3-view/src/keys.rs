//! The key table: every binding with its caption and heading, and every
//! group. It is the one source of both what a key does and what the prefix
//! menu shows: a key that holds a group opens that group's menu.
use crate::script::Verb;
use std::collections::BTreeMap;

/// What a binding does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Acts on the viewer's own thread, even while a binding runs.
    Verb(Verb),
    /// Runs the configuration's function bound under this number.
    Script(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub caption: String,
    pub heading: String,
    /// When it was bound: the menu lists in this order.
    pub order: u64,
}

/// A key that holds other keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub caption: String,
    pub heading: String,
    /// Its keys act again without the keys before them, until another key.
    pub repeat: bool,
    pub order: u64,
}

/// One line of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Heading(String),
    /// A key, its caption, and whether it opens a group.
    Key(String, String, bool),
}

#[derive(Default)]
pub struct Keys {
    pub bindings: BTreeMap<String, Binding>,
    pub groups: BTreeMap<String, Group>,
}

impl Keys {
    /// Whether some binding lies below `chord`.
    pub fn opens(&self, chord: &str) -> bool {
        let below = format!("{chord} ");
        self.bindings.keys().any(|key| key.starts_with(&below))
    }

    /// The group holding `chord`, if its keys repeat.
    pub fn repeating_parent(&self, chord: &str) -> Option<String> {
        let (parent, _) = chord.rsplit_once(' ')?;
        let group = self.groups.get(parent)?;
        group.repeat.then(|| parent.to_owned())
    }

    /// The menu of the keys right below `prefix`, under their headings in
    /// the order each heading was first used. A group shows its caption; one
    /// made only by the keys below it shows them counted.
    pub fn menu(&self, prefix: &str) -> Vec<Line> {
        let below = format!("{prefix} ");
        let mut entries: Vec<(u64, String, String, String, bool)> = Vec::new();
        for (chord, binding) in &self.bindings {
            let Some(rest) = chord.strip_prefix(&below) else {
                continue;
            };
            match rest.split_once(' ') {
                None => entries.push((
                    binding.order,
                    binding.heading.clone(),
                    rest.to_owned(),
                    binding.caption.clone(),
                    false,
                )),
                Some((key, _)) => {
                    if entries.iter().any(|e| e.2 == key && e.4) {
                        continue;
                    }
                    let chord = format!("{below}{key}");
                    let (order, heading, caption) = match self.groups.get(&chord) {
                        Some(group) => (group.order, group.heading.clone(), group.caption.clone()),
                        None => (binding.order, "More".to_owned(), format!("{key} …")),
                    };
                    entries.push((order, heading, key.to_owned(), caption, true));
                }
            }
        }
        entries.sort_by_key(|e| e.0);
        let mut headings: Vec<String> = Vec::new();
        for entry in &entries {
            if !headings.contains(&entry.1) {
                headings.push(entry.1.clone());
            }
        }
        let mut lines = Vec::new();
        for heading in headings {
            lines.push(Line::Heading(heading.clone()));
            for (_, _, key, caption, group) in entries.iter().filter(|e| e.1 == heading) {
                lines.push(Line::Key(key.clone(), caption.clone(), *group));
            }
        }
        lines
    }
}
