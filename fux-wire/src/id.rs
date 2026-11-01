//! The IDs of panes, tabs, workspaces, clients and copies. A new one comes only from
//! the session's counters, [`Ids`]; any other is read from what a person
//! typed (`%3`, `@2`, `+1`, `c1`) by `FromStr`, or decoded from a frame as
//! its number. The session looks it up before acting on it.
use crate::code::Error;
use crate::command::{Kind, Usage};
use std::fmt;
use std::str::FromStr;

/// `id!(Name, '%', Kind::Pane)` expands to `pub struct Name(u32)`, its
/// `Display` (`%N`), `number()`, `Wire` (the `u32`), and a `FromStr` that
/// reads `%N` or fails with `Usage::Not(Kind::Pane, …)`. Without a kind there is no
/// `FromStr`: the id reads its own.
macro_rules! id {
    ($(#[$doc:meta])* $name:ident, $sigil:literal $(, $kind:expr)?) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);
        crate::wire::wire_struct!($name { 0 });
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
    /// A workspace's number, `+N`.
    WsId, '+', Kind::Workspace
);
id!(
    /// An attached client's number, `cN`.
    ClientId, 'c'
);

/// Digits only: no sign, no space.
pub(crate) fn number<T: FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `cN`, or `N` alone.
impl FromStr for ClientId {
    type Err = Usage;
    fn from_str(text: &str) -> Result<Self, Usage> {
        let n = text
            .strip_prefix('c')
            .and_then(number)
            .or_else(|| number(text));
        n.map(Self).ok_or_else(|| Usage::NotClient(text.to_owned()))
    }
}

/// The last id of each kind given out, from 1: ids count up and are not
/// given twice; one that would wrap is refused. A copy taken for something
/// being made is committed back once it is made, so that a failure part
/// way uses none up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ids {
    pane: u32,
    tab: u32,
    workspace: u32,
    client: u32,
}

impl Ids {
    pub fn pane(&mut self) -> Result<PaneId, Error> {
        take(&mut self.pane, "pane").map(PaneId)
    }

    /// Whether pane id `id` was ever given in this run.
    pub fn gave(&self, id: PaneId) -> bool {
        id.0 > 0 && id.0 <= self.pane
    }
    pub fn gave_scope(&self, scope: crate::inspect::Scope) -> bool {
        let within = |n: u32, last: u32| n > 0 && n <= last;
        match scope {
            crate::inspect::Scope::All => true,
            crate::inspect::Scope::Target(crate::command::AnyRef::Pane(p)) => self.gave(p),
            crate::inspect::Scope::Target(crate::command::AnyRef::Tab(t)) => within(t.0, self.tab),
            crate::inspect::Scope::Target(crate::command::AnyRef::Workspace(w)) => {
                within(w.0, self.workspace)
            }
            crate::inspect::Scope::Client(c) => within(c.0, self.client),
        }
    }
    pub fn tab(&mut self) -> Result<TabId, Error> {
        take(&mut self.tab, "tab").map(TabId)
    }
    pub fn workspace(&mut self) -> Result<WsId, Error> {
        take(&mut self.workspace, "workspace").map(WsId)
    }
    pub fn client(&mut self) -> Result<ClientId, Error> {
        take(&mut self.client, "client").map(ClientId)
    }
}

fn take(counter: &mut u32, what: &'static str) -> Result<u32, Error> {
    *counter = (counter.checked_add(1)).ok_or_else(|| {
        crate::code::fail(
            crate::code::Raise::IdsExhausted,
            format!("no {what} IDs are left"),
        )
    })?;
    Ok(*counter)
}

/// A pane by number, for tests of what holds panes without a session.
#[cfg(any(test, feature = "testing"))]
impl PaneId {
    pub fn of(n: u32) -> PaneId {
        PaneId(n)
    }
}
/// A tab and a workspace by number, for tests of a pane alone.
#[cfg(any(test, feature = "testing"))]
impl TabId {
    pub fn of(n: u32) -> TabId {
        TabId(n)
    }
}
#[cfg(any(test, feature = "testing"))]
impl WsId {
    pub fn of(n: u32) -> WsId {
        WsId(n)
    }
}
/// A client by number, for tests of what is sent it.
#[cfg(any(test, feature = "testing"))]
impl ClientId {
    pub fn of(n: u32) -> ClientId {
        ClientId(n)
    }
}
#[cfg(any(test, feature = "testing"))]
impl Ids {
    /// Uses up the tab IDs.
    pub fn exhaust_tabs(&mut self) {
        self.tab = u32::MAX;
    }
}

/// The pane and program generation given to a pane's process as `FUX_PANE`.
pub fn program(text: &str) -> Option<(PaneId, u64)> {
    let (pane, generation) = text.split_once(':')?;
    Some((pane.parse().ok()?, number(generation)?))
}
