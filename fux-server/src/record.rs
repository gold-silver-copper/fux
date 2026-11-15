//! A run's record (D-124 (3)): its tree, written down by its server on
//! every change of it, before the change is told, so that a server that
//! takes the run back after a crash finds every id, name and layout anyone
//! saw. Its recovery state, read by no one else: fux's own, changing with
//! it (`socket::panes`).
//!
//! It is one file, `run`, written whole on every change of the tree. The
//! structure is flat: the workspaces and the tabs are two lists of rows,
//! each workspace naming its children by their ids, so no depth of
//! nesting is read or written by going into it. Names are in it, each at
//! most `MAX_NAME` bytes (`session::check_name`).
//!
//! A run has one server at a time (D-180: the lock on its directory,
//! `socket::bind_socket`), so every file here is that server's to keep or
//! remove, whichever server wrote it.
use fux_wire::command::{AnyRef, Run};
use fux_wire::id::{PaneId, TabId, WsId};
use fux_wire::layout::Node;
use std::path::{Path, PathBuf};

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

/// A workspace: its name, and what it holds, tabs and
/// workspaces, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: WsId,
    pub name: Option<String>,
    pub children: Vec<AnyRef>,
}
wired!(struct Workspace { id, name, children });

/// A tab: its layout, its revision, how it is sized, its zoomed pane, and
/// its panes.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub name: Option<String>,
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
    pub name: Option<String>,
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

/// The longest name, in bytes: the record is written whole on every
/// change, names and all, so what a name weighs is bounded.
pub const MAX_NAME: usize = 1024;

/// The structure at `path`, whole, if one is there: what a crashed server
/// left.
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

/// Writes `bytes`, a record, to `path`, whole: to a file beside it renamed
/// over it, so that a crash leaves the last one or this one, never a
/// part. That file is this process's own (`PATH.new.PID`), and one that
/// fails removes it alone. No `fsync`: a machine's crash ends the panes
/// anyway.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut new = path.as_os_str().to_owned();
    new.push(format!(".new.{}", std::process::id()));
    let new = PathBuf::from(new);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let written = (fuxix::file::private_file(&mut options).open(&new))
        // Closed before it is renamed: on Windows a file open with no
        // delete sharing (as private files are) can't be.
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&new, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written
}

impl Tree {
    /// The panes it holds.
    pub fn panes(&self) -> Vec<&Pane> {
        self.tabs.iter().flat_map(|tab| &tab.panes).collect()
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

/// What changes nothing a record holds, reached without marking the tree
/// changed: the journal and who acts in it, what a close moved, which
/// tabs are to be sized and what size a pane was asked to take, who typed
/// last. Each is asked on every request or after every event, and none
/// is in the record (`Parts::record`).
impl Tracked<crate::workspace::Parts> {
    pub(crate) fn actor(&mut self, actor: fux_wire::events::Actor) -> fux_wire::events::Actor {
        self.value.actor(actor)
    }
    pub(crate) fn journal_mut(&mut self) -> &mut crate::journal::Journal {
        self.value.journal_mut()
    }
    pub(crate) fn take_displaced(&mut self) -> Vec<(fux_wire::id::ClientId, String)> {
        self.value.take_displaced()
    }
    pub(crate) fn take_changed(&mut self) -> Vec<TabId> {
        self.value.take_changed()
    }
    pub(crate) fn typed(&mut self, tab: TabId, client: fux_wire::id::ClientId) {
        self.value.typed(tab, client);
    }
    pub(crate) fn laid_out(&mut self, tab: TabId) {
        self.value.laid_out(tab);
    }
    /// Whether pane `pane` is to be told to take `size` (`Copy::ask_size`):
    /// what it was asked is not what it is at, which alone is recorded.
    pub(crate) fn ask_size(&mut self, pane: PaneId, size: fux_vt::Size) -> bool {
        (self.value.pane_mut(pane)).is_some_and(|pane| pane.ask_size(size))
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
