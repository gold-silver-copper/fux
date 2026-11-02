//! A run's record (D-124 (3)): its tree, written down whole by its server
//! on every change of it, before the change is told, so that a server that
//! takes the run back after a crash finds every id, name and layout anyone
//! saw. Its recovery state, read by no one else: fux's own, changing with
//! it (`socket::panes`).
use crate::copy::Copy as Pane;
use fux_wire::command::Run;
use fux_wire::id::{PaneId, TabId, WsId};
use fux_wire::layout::Node;

/// What a run is: its identity, the ids it gave last, and its workspaces,
/// at the top in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub run: Run,
    pub ids: Vec<u32>,
    pub workspaces: Vec<Workspace>,
}
wired!(struct Record { run, ids, workspaces });

/// A workspace and what it holds, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: WsId,
    pub name: Option<String>,
    pub children: Vec<Child>,
}
wired!(struct Workspace { id, name, children });

#[derive(Clone, Debug, PartialEq)]
pub enum Child {
    Tab(Tab),
    Workspace(Workspace),
}
wired!(Child { Tab(t), Workspace(w) });

/// A tab: its layout, its revision, how it is sized, its zoomed pane, and
/// its panes' names.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub name: Option<String>,
    pub nodes: Vec<Node>,
    pub revision: Option<u64>,
    pub fixed: bool,
    pub size: fux_vt::Size,
    pub zoomed: Option<PaneId>,
    pub names: Vec<(PaneId, Option<String>)>,
}
wired!(struct Tab { id, name, nodes, revision, fixed, size, zoomed, names });

impl Tab {
    /// Whether it keeps its size, whatever shows it.
    pub fn fit_fixed(&self) -> bool {
        self.fixed
    }
}

/// The record at `path`, whole, if one is there: what a crashed server
/// left.
pub fn read(path: &std::path::Path) -> Result<Option<Record>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let record = fux_wire::wire::Reader::new(&bytes).whole();
    record
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes `bytes`, a record, to `path`, whole: to a file beside it renamed
/// over it, so that a crash leaves the last one or this one, never a
/// part. No `fsync`: a machine's crash ends the panes anyway.
pub fn write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut new = path.as_os_str().to_owned();
    new.push(".new");
    let new = std::path::PathBuf::from(new);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(fuxix::file::NOFOLLOW)
        .open(&new)?;
    file.write_all(bytes)?;
    std::fs::rename(&new, path)
}

/// The panes a record holds, each by its tab.
pub fn panes(workspaces: &[Workspace]) -> Vec<(TabId, PaneId)> {
    let mut panes = Vec::new();
    for ws in workspaces {
        for child in &ws.children {
            match child {
                Child::Tab(tab) => {
                    panes.extend(tab.names.iter().map(|(pane, _)| (tab.id, *pane)));
                }
                Child::Workspace(inner) => panes.extend(self::panes(std::slice::from_ref(inner))),
            }
        }
    }
    panes
}

/// Its name, as the record holds it, of each pane `panes` holds.
pub fn named(workspaces: &[Workspace], panes: &mut std::collections::BTreeMap<PaneId, Pane>) {
    for ws in workspaces {
        for child in &ws.children {
            match child {
                Child::Tab(tab) => {
                    for (id, name) in &tab.names {
                        if let (Some(pane), Some(name)) = (panes.get_mut(id), name) {
                            pane.rename(name.clone());
                        }
                    }
                }
                Child::Workspace(inner) => named(std::slice::from_ref(inner), panes),
            }
        }
    }
}
