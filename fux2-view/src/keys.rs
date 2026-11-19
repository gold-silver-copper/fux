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

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(keys: &mut Keys, chord: &str, caption: &str, heading: &str, order: u64) {
        keys.bindings.insert(
            chord.to_owned(),
            Binding {
                action: Action::Script(order),
                caption: caption.to_owned(),
                heading: heading.to_owned(),
                order,
            },
        );
    }

    #[test]
    fn a_menu_is_the_keys_below_under_their_headings() {
        let mut keys = Keys::default();
        bind(&mut keys, "C-b v", "split side by side", "Panes", 1);
        bind(&mut keys, "C-b t n", "new tab", "Tabs", 2);
        bind(&mut keys, "C-b t r", "rename tab", "Tabs", 3);
        bind(&mut keys, "C-b d", "detach", "Session", 4);
        keys.groups.insert(
            "C-b t".to_owned(),
            Group {
                caption: "tabs".to_owned(),
                heading: "Tabs".to_owned(),
                repeat: false,
                order: 0,
            },
        );
        let menu = keys.menu("C-b");
        assert_eq!(
            menu,
            [
                Line::Heading("Tabs".to_owned()),
                Line::Key("t".to_owned(), "tabs".to_owned(), true),
                Line::Heading("Panes".to_owned()),
                Line::Key("v".to_owned(), "split side by side".to_owned(), false),
                Line::Heading("Session".to_owned()),
                Line::Key("d".to_owned(), "detach".to_owned(), false),
            ]
        );
        assert_eq!(keys.menu("C-b t").len(), 3);
        assert!(keys.opens("C-b") && keys.opens("C-b t") && !keys.opens("C-b v"));
    }
}
