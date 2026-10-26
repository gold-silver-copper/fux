//! What a reply answers of panes, and of everything there is, as data the
//! client renders as text or as JSON (`json`), each from the one value.
use crate::code::Code;
use crate::command::{Kind, Version};
use crate::id::PaneId;
use crate::json::Json;
use crate::process::Ended;
use crate::wire::{closed_wire, wire, wire_enum};

wire! {
    /// What one pane answers, of an action or a wait on it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum PaneAnswer {
        /// An action's (keys, a mouse event, a signal, a program started):
        /// its pane's version before it acted, or as it starts, which a
        /// wait for its effect waits to differ from.
        Acted(version: Version),
        /// A wait's, its pane changed: its version now, what changed, and
        /// how its program ended, if it ended since.
        Changed {
            version: Version,
            changed: Vec<Change>,
            ended: Option<Ended>,
        },
        /// A wait's, its pane closed.
        Closed(pane: PaneId),
        /// A wait's, nothing changed in time: the version it is still at.
        TimedOut(since: Version),
    }
}

/// What changed of a pane a wait answers: its program started again
/// (whose other counts say nothing of the last one's), or its screen, a
/// bell, a shell command's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Respawned,
    Screen,
    Bell,
    Done,
}

impl Change {
    pub const ALL: [Change; 4] = [
        Change::Respawned,
        Change::Screen,
        Change::Bell,
        Change::Done,
    ];

    fn name(self) -> &'static str {
        match self {
            Change::Respawned => "respawned",
            Change::Screen => "screen",
            Change::Bell => "bell",
            Change::Done => "done",
        }
    }
}

closed_wire!(Change, "change");
wire_enum!(Ended { Exited(code), Signalled(signal), Lost });

impl PaneAnswer {
    pub fn pane(&self) -> PaneId {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut(version) => version.pane,
            PaneAnswer::Closed(pane) => *pane,
        }
    }

    /// The version it holds, which a wait on it waits from: none once its
    /// pane closed.
    pub fn version(&self) -> Option<Version> {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut(version) => Some(*version),
            PaneAnswer::Closed(_) => None,
        }
    }

    /// The code of the failure it is, if it is one.
    pub fn failure(&self) -> Option<Code> {
        match self {
            PaneAnswer::Acted(_) | PaneAnswer::Changed { .. } => None,
            PaneAnswer::Closed(_) => Some(Code::Closed),
            PaneAnswer::TimedOut(_) => Some(Code::Timeout),
        }
    }

    /// A line that names the pane: `%2:0.57.1.3.0:RUN screen done exited
    /// 0`, `%2 closed`, `%2:0.57.1.3.0:RUN timeout`.
    pub fn text(&self) -> String {
        match self {
            PaneAnswer::Acted(version) => version.to_string(),
            PaneAnswer::Changed {
                version,
                changed,
                ended,
            } => std::iter::once(version.to_string())
                .chain(changed.iter().map(|c| c.name().to_owned()))
                .chain(ended.map(crate::process::told))
                .collect::<Vec<_>>()
                .join(" "),
            PaneAnswer::Closed(pane) => format!("{pane} closed"),
            PaneAnswer::TimedOut(since) => format!("{since} timeout"),
        }
    }

    /// As `json` gives it: the pane, its version, what changed and how
    /// its program ended, and its status.
    pub fn json(&self) -> Json {
        let mut fields = vec![("pane", Json::str(self.pane().to_string()))];
        fields.extend(
            self.version()
                .map(|v| ("version", Json::str(v.to_string()))),
        );
        if let PaneAnswer::Changed { changed, ended, .. } = self {
            let changed = changed.iter().map(|c| Json::str(c.name()));
            fields.push(("changed", Json::Array(changed.collect())));
            fields.extend(ended.map(Ended::field));
        }
        let status = self.failure().map_or(0, |code| code.status().get());
        fields.push(("status", Json::int(status)));
        Json::object(fields)
    }
}

/// `ls`, everything (`of` none) or a pane, tab or workspace and what is
/// in it (`info`), as text from its JSON: a line each, a pane's with its
/// label, size, and pid or how its program ended; and each client's.
pub fn listing_text(of: Option<Kind>, listing: &Json) -> String {
    let field = |json: &Json, key| match json.get(key) {
        Some(Json::String(s)) => s.clone(),
        Some(Json::Null) | None => "-".into(),
        Some(other) => other.render(),
    };
    let items = |json: &Json, key| match json.get(key) {
        Some(Json::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let int = |json: &Json, key| match json.get(key) {
        Some(Json::Int(n)) => i32::try_from(*n).ok(),
        Some(Json::Uint(n)) => i32::try_from(*n).ok(),
        _ => None,
    };
    let pane = |p: &Json| {
        let label = Some(field(p, "title")).filter(|t| !t.is_empty());
        let process = match (int(p, "pid"), int(p, "exited"), int(p, "signal")) {
            (Some(pid), ..) => format!("pid {pid}"),
            (None, Some(code), _) => crate::process::told(Ended::Exited(code)),
            (None, None, Some(raw)) => crate::process::told(Ended::Signalled(raw)),
            (None, None, None) => crate::process::told(Ended::Lost),
        };
        let (id, cols, rows) = (field(p, "id"), field(p, "cols"), field(p, "rows"));
        let label = label.unwrap_or_else(|| field(p, "name"));
        format!("    {id} {label} {cols}x{rows} {process}\n")
    };
    let tab = |t: &Json| {
        let panes: String = items(t, "panes").iter().map(pane).collect();
        format!("  {} {}\n{panes}", field(t, "id"), field(t, "name"))
    };
    let workspace = |w: &Json| {
        let tabs: String = items(w, "tabs").iter().map(tab).collect();
        format!("{} {}\n{tabs}", field(w, "id"), field(w, "name"))
    };
    let client = |c: &Json| {
        let shown = ["id", "cols", "rows", "workspace", "tab", "pane"].map(|key| field(c, key));
        let [id, cols, rows, place @ ..] = shown;
        format!("client {id} {cols}x{rows} {}\n", place.join(" "))
    };
    match of {
        Some(Kind::Pane) => pane(listing),
        Some(Kind::Tab) => tab(listing),
        Some(Kind::Workspace) => workspace(listing),
        // Everything, and where a stream of events reads from.
        None => (items(listing, "workspaces").iter().map(workspace))
            .chain(items(listing, "clients").iter().map(client))
            .chain(
                (listing.get("events")).map(|_| format!("events {}\n", field(listing, "events"))),
            )
            .collect(),
    }
}
