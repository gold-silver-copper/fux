use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    Create(&'a str),
    Select(&'a str),
    Delete(&'a str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    pub workspace: String,
    pub state: &'static str,
}

pub struct WorkspaceOracle {
    live: BTreeSet<String>,
}

impl WorkspaceOracle {
    pub fn started() -> Self {
        Self {
            live: BTreeSet::from(["binary".to_owned()]),
        }
    }

    pub fn apply(&mut self, action: Action<'_>) -> Option<Transition> {
        match action {
            Action::Create(name) if self.live.insert(name.to_owned()) => Some(Transition {
                workspace: name.to_owned(),
                state: "created",
            }),
            Action::Select(name) if self.live.contains(name) => Some(Transition {
                workspace: name.to_owned(),
                state: "selected",
            }),
            Action::Delete(name) if self.live.remove(name) => Some(Transition {
                workspace: name.to_owned(),
                state: "deleted",
            }),
            _ => None,
        }
    }
}
