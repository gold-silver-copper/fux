//! The server's state as `ls --json` tells it.
use crate::fixture::Fixture;
use serde_json::Value;

pub struct Pane {
    pub id: String,
    pub rows: u16,
    pub cols: u16,
    pub pid: i32,
}

pub struct Tab {
    pub id: String,
    pub name: String,
    pub panes: Vec<Pane>,
}

pub struct Workspace {
    pub id: String,
    pub name: String,
    pub tabs: Vec<Tab>,
}

pub struct ClientView {
    pub id: String,
    pub rows: u16,
    pub cols: u16,
    pub workspace: String,
    pub tab: Option<String>,
    pub pane: Option<String>,
    pub zoom: bool,
}

pub struct World {
    pub workspaces: Vec<Workspace>,
    pub clients: Vec<ClientView>,
}

fn text(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("no {key:?} in {value}"))
}

fn size(value: &Value, key: &str) -> Result<u16, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| format!("no {key:?} size in {value}"))
}

fn list<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no {key:?} list in {value}"))
}

impl World {
    pub fn parse(json: &str) -> Result<World, String> {
        let root: Value = serde_json::from_str(json).map_err(|e| format!("ls --json: {e}"))?;
        let mut workspaces = Vec::new();
        for ws in list(&root, "workspaces")? {
            let mut tabs = Vec::new();
            for tab in list(ws, "tabs")? {
                let mut panes = Vec::new();
                for pane in list(tab, "panes")? {
                    panes.push(Pane {
                        id: text(pane, "id")?,
                        rows: size(pane, "rows")?,
                        cols: size(pane, "cols")?,
                        pid: pane
                            .get("pid")
                            .and_then(Value::as_i64)
                            .and_then(|p| i32::try_from(p).ok())
                            .unwrap_or(0),
                    });
                }
                tabs.push(Tab {
                    id: text(tab, "id")?,
                    name: text(tab, "name")?,
                    panes,
                });
            }
            workspaces.push(Workspace {
                id: text(ws, "id")?,
                name: text(ws, "name")?,
                tabs,
            });
        }
        let mut clients = Vec::new();
        for c in list(&root, "clients")? {
            clients.push(ClientView {
                id: text(c, "id")?,
                rows: size(c, "rows")?,
                cols: size(c, "cols")?,
                workspace: text(c, "workspace")?,
                tab: c.get("tab").and_then(Value::as_str).map(str::to_owned),
                pane: c.get("pane").and_then(Value::as_str).map(str::to_owned),
                zoom: c.get("zoom").and_then(Value::as_bool).unwrap_or(false),
            });
        }
        Ok(World {
            workspaces,
            clients,
        })
    }

    pub fn read(fixture: &Fixture) -> Result<World, String> {
        let out = fixture.fux(&["ls", "--json"])?;
        if out.status != 0 {
            return Err(format!("ls --json failed: {}", out.stderr));
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
    if !fuxix::process::exists(pid) {
        return false;
    }
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
}
