//! The IDs of panes, tabs, workspaces and clients. A new one comes only from
//! the session's counters, [`Ids`]; any other is read from what a person
//! typed (`%3`, `@2`, `+1`, `c1`) by `FromStr`, and the session looks it up
//! before acting on it. No code can make one from a number.
use crate::command::{Kind, Usage};
use crate::session::Error;
use std::fmt;
use std::str::FromStr;

/// A pane's number, `%N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(u32);
/// A tab's number, `@N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(u32);
/// A workspace's number, `+N`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WsId(u32);
/// An attached client's number, `cN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(u32);

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}
impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}
impl fmt::Display for WsId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "+{}", self.0)
    }
}
impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}

/// Digits only: no sign, no space.
pub(crate) fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl FromStr for PaneId {
    type Err = Usage;
    fn from_str(text: &str) -> Result<PaneId, Usage> {
        let n = text.strip_prefix('%').and_then(number);
        n.map(PaneId)
            .ok_or_else(|| Usage::Not(Kind::Pane, text.to_owned()))
    }
}
impl FromStr for TabId {
    type Err = Usage;
    fn from_str(text: &str) -> Result<TabId, Usage> {
        let n = text.strip_prefix('@').and_then(number);
        n.map(TabId)
            .ok_or_else(|| Usage::Not(Kind::Tab, text.to_owned()))
    }
}
impl FromStr for WsId {
    type Err = Usage;
    fn from_str(text: &str) -> Result<WsId, Usage> {
        let n = text.strip_prefix('+').and_then(number);
        n.map(WsId)
            .ok_or_else(|| Usage::Not(Kind::Workspace, text.to_owned()))
    }
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

impl PaneId {
    /// The number after `%`.
    pub fn number(self) -> u32 {
        self.0
    }
}
impl WsId {
    /// The number after `+`, as a workspace's default name has it.
    pub fn number(self) -> u32 {
        self.0
    }
}

/// The last ID of each kind handed out, from 1. IDs are never reused, so
/// one that would wrap round is refused. A copy taken for something being
/// made is committed back once it is made, so that a failure part way uses
/// none up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ids {
    pane: u32,
    tab: u32,
    workspace: u32,
    client: u32,
}

impl Ids {
    pub(crate) fn pane(&mut self) -> Result<PaneId, Error> {
        take(&mut self.pane, "pane").map(PaneId)
    }
    pub(crate) fn tab(&mut self) -> Result<TabId, Error> {
        take(&mut self.tab, "tab").map(TabId)
    }
    pub(crate) fn workspace(&mut self) -> Result<WsId, Error> {
        take(&mut self.workspace, "workspace").map(WsId)
    }
    pub(crate) fn client(&mut self) -> Result<ClientId, Error> {
        take(&mut self.client, "client").map(ClientId)
    }
}

fn take(counter: &mut u32, what: &'static str) -> Result<u32, Error> {
    *counter = counter.checked_add(1).ok_or(Error::IdsExhausted(what))?;
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
