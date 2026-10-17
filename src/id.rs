//! The IDs of panes, tabs, workspaces and clients. A new one comes only from
//! the session's counters, [`Ids`]; any other is read from what a person
//! typed (`%3`, `@2`, `+1`, `c1`) by `FromStr`, and the session looks it up
//! before acting on it. No code can make one from a number.
use crate::code::Code;
use crate::command::{AnyRef, Kind, Usage};
use crate::session::Error;
use std::fmt;
use std::str::FromStr;

/// `id!(Name, '%', Kind::Pane)` expands to `pub struct Name(u32)`, its
/// `Display` (`%N`), its `number()`, and a `FromStr` that reads `%N` back
/// or fails with `Usage::Not(Kind::Pane, …)`. Without a kind there is no
/// `FromStr`: the id reads its own.
macro_rules! id {
    ($(#[$doc:meta])* $name:ident, $sigil:literal $(, $kind:expr)?) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($sigil, "{}"), self.0)
            }
        }
        impl $name {
            /// The number after its sigil.
            pub fn number(self) -> u32 {
                self.0
            }
        }
        $(impl FromStr for $name {
            type Err = Usage;
            fn from_str(text: &str) -> Result<$name, Usage> {
                let n = text.strip_prefix($sigil).and_then(number);
                n.map($name).ok_or_else(|| Usage::Not($kind, text.to_owned()))
            }
        })?
    };
}

id!(
    /// A pane's number, `%N`.
    PaneId, '%', Kind::Pane
);
id!(
    /// A tab's number, `@N`.
    TabId, '@', Kind::Tab
);
id!(
    /// A workspace's number, `+N`: a workspace's default name has it.
    WsId, '+', Kind::Workspace
);
id!(
    /// An attached client's number, `cN`.
    ClientId, 'c'
);

/// Digits only: no sign, no space.
pub(crate) fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `cN`, or `N` alone.
impl FromStr for ClientId {
    type Err = Usage;
    fn from_str(text: &str) -> Result<ClientId, Usage> {
        let n = text
            .strip_prefix('c')
            .and_then(number)
            .or_else(|| number(text));
        n.map(ClientId)
            .ok_or_else(|| Usage::NotClient(text.to_owned()))
    }
}

/// The last id of each kind given out, from 1, and the next one asked for
/// (`want`): ids count up and are not given twice, except to a restore,
/// whose ids they were. One that would wrap, or jump more than `MAX_JUMP`
/// ahead, is refused. A copy taken for something being made is committed
/// back once it is made, so that a failure part way uses none up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ids {
    pane: u32,
    tab: u32,
    workspace: u32,
    client: u32,
    wanted: Wanted,
}

/// The ids a command asks to make its workspace, tab and pane with
/// (`id`, as saved sessions give them): at most one of each kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wanted {
    pub pane: Option<PaneId>,
    pub tab: Option<TabId>,
    pub workspace: Option<WsId>,
}

impl Wanted {
    /// Each id it asks for.
    pub fn each(&self) -> impl Iterator<Item = AnyRef> {
        let pane = self.pane.map(AnyRef::Pane);
        let tab = self.tab.map(AnyRef::Tab);
        [pane, tab, self.workspace.map(AnyRef::Workspace)]
            .into_iter()
            .flatten()
    }
}

impl Ids {
    pub(crate) fn pane(&mut self) -> Result<PaneId, Error> {
        let wanted = self.wanted.pane.take().map(PaneId::number);
        take(&mut self.pane, wanted, "pane").map(PaneId)
    }
    pub(crate) fn tab(&mut self) -> Result<TabId, Error> {
        let wanted = self.wanted.tab.take().map(TabId::number);
        take(&mut self.tab, wanted, "tab").map(TabId)
    }
    pub(crate) fn workspace(&mut self) -> Result<WsId, Error> {
        let wanted = self.wanted.workspace.take().map(WsId::number);
        take(&mut self.workspace, wanted, "workspace").map(WsId)
    }
    pub(crate) fn client(&mut self) -> Result<ClientId, Error> {
        take(&mut self.client, None, "client").map(ClientId)
    }

    /// The next ids made are `wanted`'s: refused if one as high as any was
    /// ever given, which would give it twice, unless `again` (a restore,
    /// whose ids these were, and are free).
    pub(crate) fn want(&mut self, wanted: Wanted, again: bool) -> Result<(), Error> {
        for id in wanted.each() {
            let (last, n) = match id {
                AnyRef::Pane(PaneId(n)) => (self.pane, n),
                AnyRef::Tab(TabId(n)) => (self.tab, n),
                AnyRef::Workspace(WsId(n)) => (self.workspace, n),
            };
            if n == 0 || (n <= last && !again) {
                return Err(Error::IdTaken(id));
            }
            // An id far ahead would leave none after it: the ids would run out.
            if n > last.saturating_add(MAX_JUMP) || n > u32::MAX - MAX_JUMP {
                let why = "too far ahead of the ids given so far, which would run out after it";
                return Err(crate::session::fail(Code::Id, format!("id {id}: {why}")));
            }
        }
        self.wanted = wanted;
        Ok(())
    }

    /// No id is asked for any more.
    pub(crate) fn unwant(&mut self) {
        self.wanted = Wanted::default();
    }
}

/// How far ahead of the last id given an asked-for one may be.
const MAX_JUMP: u32 = 1 << 16;

fn take(counter: &mut u32, wanted: Option<u32>, what: &'static str) -> Result<u32, Error> {
    if let Some(n) = wanted {
        *counter = (*counter).max(n);
        return Ok(n);
    }
    *counter = (counter.checked_add(1)).ok_or_else(|| {
        crate::session::fail(Code::IdsExhausted, format!("no {what} IDs are left"))
    })?;
    Ok(*counter)
}

/// A pane by number, for tests of what holds panes without a session.
#[cfg(test)]
impl PaneId {
    pub(crate) fn of(n: u32) -> PaneId {
        PaneId(n)
    }
}
#[cfg(test)]
impl Ids {
    /// Uses up the tab IDs.
    pub(crate) fn exhaust_tabs(&mut self) {
        self.tab = u32::MAX;
    }
}
