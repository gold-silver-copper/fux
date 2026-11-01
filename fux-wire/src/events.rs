//! What `events` answers: finite batches of the journal's entries, each
//! with the cursor after it, read from a cursor in one server run.
use crate::command::Run;
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::json::Json;
use crate::wire::wire;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Seq(pub u64);
crate::wire::wire_struct!(Seq { 0 });

/// The next entry to consume, in exactly one server run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub seq: Seq,
    pub run: Run,
}
crate::wire::wire_struct!(Cursor { seq, run });
crate::wire::shaped!(
    struct Cursor {
        seq: Seq,
        run: Run,
    }
);
impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.seq.0, self.run)
    }
}
impl std::str::FromStr for Cursor {
    type Err = ();
    fn from_str(text: &str) -> Result<Self, ()> {
        let (seq, run) = text.split_once(':').ok_or(())?;
        let cursor = Self {
            seq: Seq(crate::id::number(seq).ok_or(())?),
            run: run.parse()?,
        };
        (cursor.to_string() == text).then_some(cursor).ok_or(())
    }
}

wire! {
/// Supplied context, never authenticated human or agent identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Actor {
    Outside,
    Client(client: ClientId),
    Pane(pane: PaneId, generation: u64),
    Program(pane: PaneId, generation: u64),
    #[default]
    Server,
}
}
impl Actor {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Outside => "outside",
            Self::Server => "server",
            Self::Client(_) => "client",
            Self::Pane(..) => "pane",
            Self::Program(..) => "program",
        }
    }
}

wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Item {
        pub data: Json,
        pub pane: Option<PaneId>,
        pub tab: Option<TabId>,
        pub workspace: Option<WsId>,
        pub client: Option<ClientId>,
        pub moved_from: Option<(WsId, TabId)>,
        pub origin: Actor,
        pub next: Cursor,
    }
}

wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Status { Continuing, Timeout, Gone, Stopped(reason: String), }
}
impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Continuing => "continuing",
            Self::Timeout => "timeout",
            Self::Gone => "gone",
            Self::Stopped(_) => "stopped",
        }
    }
}
wire! {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Batch {
        pub events: Vec<Item>,
        /// The oldest entry the journal still holds, or `next` when it
        /// holds none.
        pub first: Cursor,
        pub next: Cursor,
        pub missed: u64,
        pub status: Status,
    }
}

#[cfg(any(test, feature = "testing"))]
impl Batch {
    pub fn json(&self) -> Json {
        let events = self
            .events
            .iter()
            .map(|item| {
                let mut fields = Vec::new();
                if let Some(event) = item.data.get("event") {
                    fields.push(("event".into(), event.clone()));
                }
                for (key, value) in [
                    ("pane", item.pane.map(|id| id.to_string())),
                    ("tab", item.tab.map(|id| id.to_string())),
                    ("workspace", item.workspace.map(|id| id.to_string())),
                    ("client", item.client.map(|id| id.to_string())),
                    ("from_tab", item.moved_from.map(|(_, tab)| tab.to_string())),
                    (
                        "from_workspace",
                        item.moved_from.map(|(ws, _)| ws.to_string()),
                    ),
                ] {
                    if let Some(value) = value {
                        fields.push((key.into(), Json::str(value)));
                    }
                }
                if let Json::Object(data) = &item.data {
                    fields.extend(data.iter().filter(|(k, _)| k != "event").cloned());
                }
                let mut origin = vec![("kind", Json::str(item.origin.kind()))];
                match item.origin {
                    Actor::Client(c) => origin.push(("client", Json::str(c.to_string()))),
                    Actor::Pane(p, generation) | Actor::Program(p, generation) => {
                        origin.push(("pane", Json::str(p.to_string())));
                        origin.push(("generation", Json::uint(generation)));
                    }
                    Actor::Outside | Actor::Server => {}
                }
                fields.push(("origin".into(), Json::object(origin)));
                fields.push(("next".into(), Json::str(item.next.to_string())));
                Json::Object(fields)
            })
            .collect();
        let mut fields = vec![
            ("events", Json::Array(events)),
            ("first", Json::str(self.first.to_string())),
            ("next", Json::str(self.next.to_string())),
            ("missed", Json::uint(self.missed)),
            ("status", Json::str(self.status.name())),
        ];
        if let Status::Stopped(reason) = &self.status {
            fields.push(("reason", Json::str(reason)));
        }
        Json::object(fields)
    }
}
