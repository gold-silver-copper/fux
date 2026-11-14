//! A run's record (D-124 (3)): its tree, written down whole by its server
//! on every change of it, before the change is told, so that a server that
//! takes the run back after a crash finds every id, name and layout anyone
//! saw. Its recovery state, read by no one else: fux's own, changing with
//! it (`socket::panes`).
use fux_wire::command::Run;
use fux_wire::id::{PaneId, TabId, WsId};
use fux_wire::layout::Node;

/// What a run is: its identity, the ids it gave last, and its workspaces,
/// at the top in order; and whether it ended (a stop, or its last pane's
/// close): then no server takes it back, and the next to find it ends what
/// is left of it (`server::serve`).
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub run: Run,
    pub ids: Vec<u32>,
    pub workspaces: Vec<Workspace>,
    pub ended: bool,
}
wired!(struct Record { run, ids, workspaces, ended });

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
    let mut new = path.as_os_str().to_owned();
    new.push(".new");
    let new = std::path::PathBuf::from(new);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = fuxix::file::private_file(&mut options).open(&new)?;
    file.write_all(bytes)?;
    // Closed first: on Windows a file open with no delete sharing (as
    // private files are) can't be renamed.
    drop(file);
    std::fs::rename(&new, path)
}

/// The panes a record holds.
pub fn panes(workspaces: &[Workspace]) -> Vec<&Pane> {
    let mut panes = Vec::new();
    for ws in workspaces {
        for child in &ws.children {
            match child {
                Child::Tab(tab) => panes.extend(&tab.panes),
                Child::Workspace(inner) => panes.extend(self::panes(std::slice::from_ref(inner))),
            }
        }
    }
    panes
}
