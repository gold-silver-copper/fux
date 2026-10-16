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

/// The last ID of each kind handed out, from 1. IDs are never reused, so
/// one that would wrap round is refused. A copy taken for something being
/// made is committed back once it is made, so that a failure part way uses
/// none up.
/// The last id of each kind given out, and the next asked for (`id`):
/// ids count up, and none is given twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ids {
    pane: u32,
    tab: u32,
    workspace: u32,
    client: u32,
    wanted: Wanted,
}

/// The next id of each kind, as asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Wanted {
    pane: Option<u32>,
    tab: Option<u32>,
    workspace: Option<u32>,
}

impl Ids {
    pub(crate) fn pane(&mut self) -> Result<PaneId, Error> {
        take(&mut self.pane, &mut self.wanted.pane, "pane").map(PaneId)
    }
    pub(crate) fn tab(&mut self) -> Result<TabId, Error> {
        take(&mut self.tab, &mut self.wanted.tab, "tab").map(TabId)
    }
    pub(crate) fn workspace(&mut self) -> Result<WsId, Error> {
        take(&mut self.workspace, &mut self.wanted.workspace, "workspace").map(WsId)
    }
    pub(crate) fn client(&mut self) -> Result<ClientId, Error> {
        take(&mut self.client, &mut None, "client").map(ClientId)
    }

    /// The next id of its kind is `id`: refused if one as high was ever
    /// given, which would give it twice, unless `again` (a restore, whose
    /// ids these were, and are free).
    pub(crate) fn want(&mut self, id: AnyRef, again: bool) -> Result<(), Error> {
        let (last, wanted, n) = match id {
            AnyRef::Pane(PaneId(n)) => (self.pane, &mut self.wanted.pane, n),
            AnyRef::Tab(TabId(n)) => (self.tab, &mut self.wanted.tab, n),
            AnyRef::Workspace(WsId(n)) => (self.workspace, &mut self.wanted.workspace, n),
        };
        if n == 0 || (n <= last && !again) {
            return Err(Error::IdTaken(id));
        }
        // An id far ahead would leave none after it: the ids would run out.
        if n > last.saturating_add(MAX_JUMP) || n > u32::MAX - MAX_JUMP {
            let why = "too far ahead of the ids given so far, which would run out after it";
            return Err(crate::session::fail(Code::Id, format!("id {id}: {why}")));
        }
        *wanted = Some(n);
        Ok(())
    }

    /// No id is asked for any more.
    pub(crate) fn unwant(&mut self) {
        self.wanted = Wanted::default();
    }
}

/// How far ahead of the last id given an asked-for one may be.
const MAX_JUMP: u32 = 1 << 16;

fn take(counter: &mut u32, wanted: &mut Option<u32>, what: &'static str) -> Result<u32, Error> {
    if let Some(n) = wanted.take() {
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
