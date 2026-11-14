//! A run's record (D-124 (3)): its tree, written down by its server on
//! every change of it, before the change is told, so that a server that
//! takes the run back after a crash finds every id, name and layout anyone
//! saw. Its recovery state, read by no one else: fux's own, changing with
//! it (`socket::panes`).
//!
//! It is one file of the structure, `run`, and one file for each name
//! (D-175). The structure is flat: the workspaces and the tabs are two
//! lists of rows, each workspace naming its children by their ids, so no
//! depth of nesting is read or written by going into it. It holds no name,
//! only each name's number: a name's text is in the file `name.N`, written
//! once, when the name is given, and the structure, written whole on every
//! change of the tree, costs what its rows do, whatever the names weigh.
//!
//! The order keeps every crash recoverable: a new name's file is written
//! before the structure that names it, and a name no longer used is
//! removed after the structure that dropped it. So the structure there
//! never names a file that is not; what a crash leaves over is a name file
//! no structure names, which the server that takes the run back removes
//! (`sweep`).
use fux_wire::command::{AnyRef, Run};
use fux_wire::id::{PaneId, TabId, WsId};
use fux_wire::layout::Node;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What a run is: its identity, the ids it gave last, and its tree; and
/// whether it ended (a stop, or its last pane's close): then no server
/// takes it back, and the next to find it ends what is left of it
/// (`server::serve`).
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub run: Run,
    pub ids: Vec<u32>,
    pub tree: Tree,
    pub ended: bool,
}
wired!(struct Record { run, ids, tree, ended });

/// A run's tree: the workspaces at the top, in order, then every
/// workspace and every tab, a row each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub top: Vec<WsId>,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
}
wired!(struct Tree { top, workspaces, tabs });

/// A workspace: its name's number, and what it holds, tabs and
/// workspaces, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: WsId,
    pub name: Option<u64>,
    pub children: Vec<AnyRef>,
}
wired!(struct Workspace { id, name, children });

/// A tab: its layout, its revision, how it is sized, its zoomed pane, and
/// its panes.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub name: Option<u64>,
    pub nodes: Vec<Node>,
    pub revision: Option<u64>,
    pub fixed: bool,
    pub size: fux_vt::Size,
    pub zoomed: Option<PaneId>,
    pub panes: Vec<Pane>,
}
wired!(struct Tab { id, name, nodes, revision, fixed, size, zoomed, panes });

/// A pane, as much as its server keeps: enough to make its copy again
/// should its own process be gone when a server takes the run back.
#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    pub id: PaneId,
    pub name: Option<u64>,
    /// Its role: a shell, which closes as it ends, or a program, which
    /// stays.
    pub shell: bool,
    pub argv: fux_wire::command::Program,
    pub cwd: std::path::PathBuf,
    pub options: fux_wire::command::PaneOptions,
    pub size: fux_vt::Size,
    pub respawns: u64,
    /// How its program ended, if it has, and the counts it ended at; and
    /// how reading its terminal failed, if it did.
    pub ended: Option<(fux_wire::command::Counts, fux_wire::answer::Ended)>,
    pub read_error: Option<String>,
}
wired!(struct Pane { id, name, shell, argv, cwd, options, size, respawns, ended, read_error });

impl Tab {
    /// Whether it keeps its size, whatever shows it.
    pub fn fit_fixed(&self) -> bool {
        self.fixed
    }
}

/// The file beside a run's record that says the run ended, where the
/// record itself could not be written to say so (`Session::end_run`).
pub const ENDED: &str = "ended";

/// The structure at `path`, whole, if one is there: what a crashed server
/// left. Its names are read apart (`read_names`).
pub fn read(path: &Path) -> Result<Option<Record>, String> {
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

/// Where name `id`'s text is kept in `panes`, a run's directory.
pub fn name_file(panes: &Path, id: u64) -> PathBuf {
    panes.join(format!("{NAME}{id}"))
}

/// What every name's file is named by, before its number.
const NAME: &str = "name.";

/// The text of each name `tree` has, by its number, read from `panes`:
/// one that is not there, or is no text, makes the record unreadable.
pub fn read_names(panes: &Path, tree: &Tree) -> Result<BTreeMap<u64, Rc<str>>, String> {
    let mut names = BTreeMap::new();
    for (_, id) in tree.named() {
        let path = name_file(panes, id);
        let text = std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        names.insert(id, Rc::from(text));
    }
    Ok(names)
}

/// Removes from `panes` each name file but those of `kept`: what a crash
/// left between a name's writing and the structure's, or between the
/// structure's and an old name's removal, and a part of one, too. What
/// can't be removed is left, and logged: no structure names it.
pub fn sweep(panes: &Path, kept: &BTreeSet<u64>) {
    let Ok(entries) = std::fs::read_dir(panes) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(number) = name.to_str().and_then(|name| name.strip_prefix(NAME)) else {
            continue;
        };
        let kept = (number.parse().ok())
            .is_some_and(|id: u64| kept.contains(&id) && id.to_string() == number);
        if !kept && let Err(error) = std::fs::remove_file(entry.path()) {
            log!("{}: {error}", entry.path().display());
        }
    }
}

/// Writes `bytes`, a record, to `path`, whole: to a file beside it renamed
/// over it, so that a crash leaves the last one or this one, never a
/// part. No `fsync`: a machine's crash ends the panes anyway.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut new = path.as_os_str().to_owned();
    new.push(".new");
    let new = PathBuf::from(new);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = fuxix::file::private_file(&mut options).open(&new)?;
    file.write_all(bytes)?;
    // Closed first: on Windows a file open with no delete sharing (as
    // private files are) can't be renamed.
    drop(file);
    std::fs::rename(&new, path)
}

impl Tree {
    /// The panes it holds.
    pub fn panes(&self) -> Vec<&Pane> {
        self.tabs.iter().flat_map(|tab| &tab.panes).collect()
    }

    /// Each name's number, with what it names.
    pub fn named(&self) -> impl Iterator<Item = (AnyRef, u64)> + '_ {
        let workspaces = (self.workspaces.iter()).map(|ws| (AnyRef::Workspace(ws.id), ws.name));
        let tabs = self.tabs.iter().flat_map(|tab| {
            let panes = tab
                .panes
                .iter()
                .map(|pane| (AnyRef::Pane(pane.id), pane.name));
            std::iter::once((AnyRef::Tab(tab.id), tab.name)).chain(panes)
        });
        (workspaces.chain(tabs)).filter_map(|(of, name)| Some((of, name?)))
    }
}

/// What a record is written from, and whether it was handed out to be
/// changed since it was last written down (`settled`): every change goes
/// through `DerefMut`, so none is missed, and one that changed nothing
/// costs an encoding, not a write.
pub struct Tracked<T> {
    value: T,
    stale: bool,
}

impl<T> Tracked<T> {
    /// `value`, not written down yet.
    pub fn new(value: T) -> Tracked<T> {
        Tracked { value, stale: true }
    }
    /// Whether it may have changed since it was last written down.
    pub fn stale(this: &Tracked<T>) -> bool {
        this.stale
    }
    /// It is written down as it is.
    pub fn settled(this: &mut Tracked<T>) {
        this.stale = false;
    }
}

impl<T> std::ops::Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for Tracked<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.stale = true;
        &mut self.value
    }
}
