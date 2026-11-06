//! The server's state, as a grift program lists it a line a thing.
use crate::fixture::Fixture;

pub struct Pane {
    pub id: String,
    pub rows: u16,
    pub cols: u16,
    pub pid: i32,
}

pub struct Tab {
    pub id: String,
    pub label: String,
    pub panes: Vec<Pane>,
    /// The pane that fills it on every screen showing it, if one does.
    pub zoomed: Option<String>,
}

pub struct Workspace {
    pub id: String,
    pub label: String,
    pub tabs: Vec<Tab>,
}

pub struct ClientView {
    pub id: String,
    pub rows: u16,
    pub cols: u16,
    pub workspace: Option<String>,
    pub tab: Option<String>,
}

pub struct World {
    pub workspaces: Vec<Workspace>,
    pub clients: Vec<ClientView>,
}

/// Prints what the walk reads of the session: a line for each workspace,
/// tab, pane and client, its fields split by tabs and a label (which holds
/// no control character) last, or `none`.
const LISTING: &str = r#"(let! ((session (list-session)))
  (define! row (lambda! (kind . fields)
    (for-each (lambda! (field) (display field "\t")) (pair kind fields))
    (display "\n")))
  (for-each (lambda! (w) (row "workspace" (get w "id") (get w "label")))
    (get session "workspaces"))
  (for-each (lambda! (t)
      (row "tab" (get t "id") (get t "workspace") (get t "zoomed") (get t "label")))
    (get session "tabs"))
  (for-each (lambda! (p)
      (row "pane" (get p "id") (get p "tab") (get p "rows") (get p "columns")
        (get (get p "program") "process-id")))
    (get session "panes"))
  (for-each (lambda! (c)
      (row "client" (get c "id") (get c "rows") (get c "columns")
        (get c "workspace") (get c "tab")))
    (get session "clients")))"#;

/// A listed field that may be `none`.
fn maybe(field: &str) -> Option<String> {
    (field != "none").then(|| field.to_owned())
}

fn size(field: &str) -> Result<u16, String> {
    field.parse().map_err(|e| format!("{field:?}: {e}"))
}

impl World {
    pub fn parse(listing: &str) -> Result<World, String> {
        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut panes = Vec::new();
        let mut clients = Vec::new();
        for line in listing.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["workspace", id, label, ""] => workspaces.push(Workspace {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                    tabs: Vec::new(),
                }),
                ["tab", id, workspace, zoomed, label, ""] => tabs.push((
                    *workspace,
                    *id,
                    Tab {
                        id: (*id).to_owned(),
                        label: (*label).to_owned(),
                        panes: Vec::new(),
                        zoomed: maybe(zoomed),
                    },
                )),
                ["pane", id, tab, rows, cols, pid, ""] => panes.push((
                    *tab,
                    Pane {
                        id: (*id).to_owned(),
                        rows: size(rows)?,
                        cols: size(cols)?,
                        pid: pid.parse().unwrap_or(0),
                    },
                )),
                ["client", id, rows, cols, workspace, tab, ""] => clients.push(ClientView {
                    id: (*id).to_owned(),
                    rows: size(rows)?,
                    cols: size(cols)?,
                    workspace: maybe(workspace),
                    tab: maybe(tab),
                }),
                _ => return Err(format!("a listing line {line:?}")),
            }
        }
        for (workspace, id, mut tab) in tabs {
            let (held, rest): (Vec<_>, Vec<_>) = panes.into_iter().partition(|(t, _)| *t == id);
            panes = rest;
            tab.panes = held.into_iter().map(|(_, pane)| pane).collect();
            (workspaces.iter_mut())
                .find(|w| w.id == workspace)
                .ok_or_else(|| format!("tab {id} of no workspace {workspace}"))?
                .tabs
                .push(tab);
        }
        Ok(World {
            workspaces,
            clients,
        })
    }

    pub fn read(fixture: &Fixture) -> Result<World, String> {
        let out = fixture.fux(&[LISTING])?;
        if out.status != 0 {
            return Err(format!("listing the session failed: {}", out.stderr));
        }
        World::parse(&out.stdout)
    }

    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.workspaces.iter().flat_map(|w| w.tabs.iter())
    }

    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.tabs().flat_map(|t| t.panes.iter())
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs().find(|t| t.id == id)
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }

    pub fn pane(&self, id: &str) -> Option<&Pane> {
        self.panes().find(|p| p.id == id)
    }
}

/// Whether a process exists and has not exited: a zombie waiting for its
/// parent to reap it counts as gone.
pub fn alive(pid: fuxix::process::Pid) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
}
