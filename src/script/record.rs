//! Immutable local projections. Only checked constructors can retain a record.
use std::rc::Rc;
use super::{Fux, handle};
use fux_wire::code::{Code, Raise};
use fux_wire::command::{AnyRef, Run};
use fux_wire::inspect::{self, Projection, Scope};
use fux_wire::json::Json;
use fuxth::{Failure, List, Value};

type V = Value<Fux>;

#[derive(Clone, Debug)]
pub struct Record {
    pub(super) fields: Vec<(String, V)>,
    depth: u8,
    pub(super) of: Option<Run>,
}

impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}

impl Record {
    pub(super) fn get(&self, field: &str) -> Result<V, Failure> {
        self.fields
            .iter()
            .find(|(key, _)| key == field)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| {
                Failure::new(
                    Code::from(Raise::NoField).into(),
                    format!("record has no field {field:?}"),
                )
            })
    }
    pub(super) fn depth(&self) -> u8 {
        self.depth
    }
    pub(super) fn write(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        out.write_char('{')?;
        for (i, (key, value)) in self.fields.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            fux_wire::json::write_str(out, key)?;
            out.write_char(':')?;
            super::words::write_json_value(value, out)?;
        }
        out.write_char('}')
    }
}

fn fail() -> Failure {
    Failure::new(
        fuxth::Code::TooLong,
        "record nests deeper than a record may",
    )
}
fn null() -> V {
    Value::App(Fux::Null)
}
fn number(n: impl TryInto<i64>) -> Result<V, Failure> {
    n.try_into().map(Value::Int).map_err(|_| {
        Failure::new(
            Code::from(Raise::Range).into(),
            "integer is past fuxth's range",
        )
    })
}
fn id(id: AnyRef, run: Run) -> V {
    Value::App(handle(id, Some(run)))
}

/// What builds records and the values in them.
struct Builder;
impl Builder {
    fn text(&mut self, text: &str) -> Result<V, Failure> {
        Ok(Value::str(text))
    }
    fn optional(&mut self, text: Option<&str>) -> Result<V, Failure> {
        text.map(|s| self.text(s)).unwrap_or_else(|| Ok(null()))
    }
    fn list<T>(
        &mut self,
        items: &[T],
        mut value: impl FnMut(&mut Self, &T) -> Result<V, Failure>,
    ) -> Result<V, Failure> {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(value(self, item)?);
        }
        let list = List::new(out).ok_or_else(fail)?;
        Ok(Value::List(Rc::new(list)))
    }
    fn record<const N: usize>(&mut self, fields: [(&str, V); N]) -> Result<V, Failure> {
        let mut out = Vec::with_capacity(N);
        for (key, value) in fields {
            out.push((key.to_owned(), value));
        }
        self.finish(out, None)
    }
    fn finish(&mut self, fields: Vec<(String, V)>, of: Option<Run>) -> Result<V, Failure> {
        let depth = fields.iter().map(|(_, value)| value.depth()).max();
        let depth = (depth.unwrap_or(0).checked_add(1))
            .filter(|d| *d <= inspect::DEPTH)
            .ok_or_else(fail)?;
        Ok(Value::App(Fux::Record(Rc::new(Record { fields, depth, of }))))
    }
    fn json(&mut self, value: &Json, depth: u8) -> Result<V, Failure> {
        if depth >= inspect::DEPTH {
            return Err(fail());
        }
        match value {
            Json::Null => Ok(null()),
            Json::Bool(b) => Ok(Value::Bool(*b)),
            Json::Int(n) => Ok(Value::Int(*n)),
            Json::Uint(n) => number(*n),
            Json::String(s) => self.text(s),
            Json::Array(items) => self.list(items, |b, v| b.json(v, depth.saturating_add(1))),
            Json::Object(fields) => {
                let mut out = Vec::with_capacity(fields.len());
                for (key, value) in fields {
                    let key = key.to_owned();
                    let value = self.json(value, depth.saturating_add(1))?;
                    out.push((key, value));
                }
                self.finish(out, None)
            }
        }
    }
    fn path(&mut self, path: &inspect::Path) -> Result<V, Failure> {
        let (encoding, text) = match path {
            inspect::Path::Utf8(text) => ("utf8", self.text(text)?),
            inspect::Path::Bytes(bytes) => {
                let text: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                ("hex", self.text(&text)?)
            }
        };
        let encoding = self.text(encoding)?;
        self.record([("encoding", encoding), ("text", text)])
    }
}

pub(super) fn from_fields(fields: Vec<(String, V)>, of: Option<Run>) -> Result<V, Failure> {
    Builder.finish(fields, of)
}

pub(super) fn from_json(value: &Json) -> Result<V, Failure> {
    Builder.json(value, 0)
}

pub(super) fn events(batch: fux_wire::events::Batch) -> Result<V, Failure> {
    use fux_wire::events::{Actor, Status};
    let run = batch.next.run;
    let reason = match &batch.status {
        Status::Stopped(reason) => Some(reason.as_str()),
        Status::Continuing | Status::Timeout | Status::Gone => None,
    };
    let mut b = Builder;
    let events = b.list(&batch.events, |b, item| {
        let kind = b.text(item.origin.kind())?;
        let origin = match item.origin {
            Actor::Client(c) => b.record([
                ("kind", kind),
                ("client", Value::App(Fux::Client(c, Some(run)))),
            ])?,
            Actor::Pane(p, generation) | Actor::Program(p, generation) => b.record([
                ("kind", kind),
                ("pane", id(AnyRef::Pane(p), run)),
                ("generation", number(generation)?),
            ])?,
            Actor::Server | Actor::Unknown => b.record([("kind", kind)])?,
        };
        let next = b.text(&item.next.to_string())?;
        let at = |r: Option<AnyRef>| r.map(|r| id(r, run));
        let (from_workspace, from_tab) = item.moved_from.unzip();
        let ids = [
            ("pane", at(item.pane.map(AnyRef::Pane))),
            ("tab", at(item.tab.map(AnyRef::Tab))),
            ("workspace", at(item.workspace.map(AnyRef::Workspace))),
            (
                "client",
                item.client.map(|c| Value::App(Fux::Client(c, Some(run)))),
            ),
            ("from_tab", at(from_tab.map(AnyRef::Tab))),
            ("from_workspace", at(from_workspace.map(AnyRef::Workspace))),
        ];
        let Json::Object(data) = &item.data else {
            return Err(fail());
        };
        let count = data
            .len()
            .saturating_add(ids.iter().filter(|(_, v)| v.is_some()).count())
            .saturating_add(2);
        let mut out = Vec::with_capacity(count);
        // The producer puts the event name first; IDs remain typed throughout.
        for (key, value) in data.iter().take(1) {
            out.push((key.to_owned(), b.json(value, 0)?));
        }
        for (key, value) in ids {
            if let Some(value) = value {
                out.push((key.to_owned(), value));
            }
        }
        for (key, value) in data.iter().skip(1) {
            out.push((key.to_owned(), b.json(value, 0)?));
        }
        out.push(("origin".to_owned(), origin));
        out.push(("next".to_owned(), next));
        b.finish(out, None)
    })?;
    let next = b.text(&batch.next.to_string())?;
    let status = b.text(batch.status.name())?;
    let reason = b.optional(reason)?;
    b.record([
        ("events", events),
        ("next", next),
        ("missed", number(batch.missed)?),
        ("status", status),
        ("reason", reason),
    ])
}

macro_rules! record {
    ($b:ident; $($key:literal => $value:expr),* $(,)?) => {{
        let fields = [$(( $key, $value )),*];
        $b.record(fields)
    }};
}

pub(super) fn snapshot(s: inspect::Snapshot) -> Result<V, Failure> {
    let b = &mut Builder;
    let run = s.run;
    let workspaces = b.list(&s.workspaces, |b,w| record! { b;
        "id" => id(AnyRef::Workspace(w.id), run), "name" => b.optional(w.name.as_deref())?,
        "label" => b.text(&w.label)?, "tabs" => b.list(&w.tabs, |_,t| Ok(id(AnyRef::Tab(*t), run)))?
    })?;
    let tabs = b.list(&s.tabs, |b,t| {
        let nodes = b.list(&t.nodes, |b,n| match n {
            fux_wire::layout::Node::Pane(p) => Ok(id(AnyRef::Pane(*p), run)),
            fux_wire::layout::Node::Split { axis, first, second, first_share, second_share } => {
                let row = [b.text(match axis { fux_wire::layout::Axis::Horizontal => "horizontal", fux_wire::layout::Axis::Vertical => "vertical" })?, number(first_share.get())?, number(second_share.get())?, number(*first)?, number(*second)?];
                b.list(&row, |_,v| Ok(v.clone()))
            }
        })?;
        record! { b; "id" => id(AnyRef::Tab(t.id),run), "workspace" => id(AnyRef::Workspace(t.workspace),run),
            "name" => b.optional(t.name.as_deref())?, "label" => b.text(&t.label)?,
            "fit" => b.text(if t.fixed { "fixed" } else { "clients" })?, "cols" => number(t.cols)?, "rows" => number(t.rows)?,
            "host" => t.host.map(|c| Value::App(Fux::Client(c,Some(run)))).unwrap_or_else(null),
            "nodes" => nodes, "layout_revision" => t.revision.map(|r| b.text(&r.to_string())).unwrap_or_else(|| Ok(null()))?
        }
    })?;
    let panes = b.list(&s.panes, |b,p| {
        let base = [
            ("id", id(AnyRef::Pane(p.id),run)), ("tab", id(AnyRef::Tab(p.tab),run)),
            ("name", b.optional(p.name.as_deref())?), ("label", b.text(&p.label)?), ("title", b.text(&p.title)?),
            ("pid", p.pid.map(number).unwrap_or_else(|| Ok(null()))?),
            ("rows", number(p.rows)?), ("cols", number(p.cols)?), ("version", b.text(&p.version.to_string())?),
        ];
        let mut fields = Vec::with_capacity(base.len().saturating_add(2));
        for (key, value) in base { fields.push((key.to_owned(), value)); }
        if let Some(state) = &p.state { fields.push(("state".to_owned(), b.json(state, 0)?)); }
        if let Some(r) = &p.recipe {
            let env = b.list(&r.env, |b,(key,value)| record! { b; "name" => b.text(key)?, "value" => value.as_ref().map(|p| b.path(p)).unwrap_or_else(|| Ok(null()))? })?;
            let recipe = record! { b; "choice" => b.text(if r.shell { "shell" } else { "program" })?,
                "argv" => b.list(&r.argv, |b,s| b.text(s))?, "env" => env, "initial_cwd" => b.path(&r.initial_cwd)?,
                "observed_cwd" => match &r.observed_cwd {
                    inspect::Directory::Known(path) => record! { b; "status" => b.text("known")?, "path" => b.path(path)? }?,
                    inspect::Directory::Unknown => null(),
                    inspect::Directory::Failed(errno) => record! { b; "status" => b.text("failed")?, "operation" => b.text("cwd")?, "errno" => number(*errno)? }?,
                },
                "history_bytes" => number(r.options.history_bytes.get())?, "program_clipboard" => Value::Bool(r.options.program_clipboard),
                "reflow" => Value::Bool(r.options.reflow), "alternate_scroll_lines" => number(r.options.alternate_scroll_lines.get())?, "picture_bytes" => number(r.options.picture_bytes.get())?
            }?;
            fields.push(("launch".to_owned(), recipe));
        }
        b.finish(fields, None)
    })?;
    let clients = b.list(&s.clients, |b,c| record! { b;
        "id" => Value::App(Fux::Client(c.id,Some(run))), "kind" => b.text(if c.terminal { "terminal" } else { "headless" })?,
        "terminal" => b.json(&c.terminal_facts, 0)?, "output" => b.json(&c.output, 0)?,
        "read_only" => Value::Bool(c.read_only), "rows" => number(c.rows)?, "cols" => number(c.cols)?,
        "pane_rows" => number(c.pane_rows)?, "pane_cols" => number(c.pane_cols)?,
        "workspace" => c.workspace.map(|w| id(AnyRef::Workspace(w),run)).unwrap_or_else(null),
        "tab" => c.tab.map(|t| id(AnyRef::Tab(t),run)).unwrap_or_else(null), "pane" => c.pane.map(|p| id(AnyRef::Pane(p),run)).unwrap_or_else(null),
        "zoom" => Value::Bool(c.zoom), "opening" => c.opening.map(number).unwrap_or_else(|| Ok(null()))?, "mode" => b.text(&c.mode)?
    })?;
    let scope = match s.scope {
        Scope::All => Value::App(Fux::All),
        Scope::Target(t) => id(t, run),
        Scope::Client(c) => Value::App(Fux::Client(c, Some(run))),
    };
    let value = record! { b; "run" => b.text(&run.to_string())?, "cursor" => b.text(&s.cursor.to_string())?, "scope" => scope,
        "projection" => b.text(match &s.projection { Projection::Facts => "facts", Projection::Topology => "topology", Projection::Recipe => "recipe", Projection::Field(_) => return Err(fail()) })?,
        "workspaces" => workspaces, "tabs" => tabs, "panes" => panes, "clients" => clients
    }?;
    let Value::App(Fux::Record(mut record)) = value else {
        return Err(fail());
    };
    Rc::get_mut(&mut record).ok_or_else(fail)?.of = Some(run);
    Ok(Value::App(Fux::Record(record)))
}

/// `fields!(Ty { Variant "name", … })`: the fields `get` reads of a held
/// value of one kind, an enum, and `Ty::ALL`, each with its name: what
/// `get` reads by, and what `docs/records.txt` lists.
macro_rules! fields {
    ($(#[$doc:meta])* $ty:ident { $($var:ident $name:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Clone, Copy)]
        pub(super) enum $ty {
            $($var),+
        }
        impl $ty {
            pub(super) const ALL: &[($ty, &str)] = &[$(($ty::$var, $name)),+];
            fn named(field: &str) -> Option<$ty> {
                Self::ALL.iter().find(|(_, name)| *name == field).map(|(f, _)| *f)
            }
        }
    };
}

fields!(
    /// A held action's or wait's answer on one pane: `changed`, and
    /// `exited` or `signal` as its program ended, only of a wait that saw
    /// a change.
    AnswerField {
        Pane "pane",
        Version "version",
        Status "status",
        Changed "changed",
        Exited "exited",
        Signal "signal",
    }
);
fields!(
    /// A capture's own: `styles` only of one read with styles.
    CaptureField {
        ScreenRows "screen_rows",
        ScreenCols "screen_cols",
        ScreenCursor "screen_cursor",
        Lines "lines",
        Styles "styles",
    }
);
fields!(
    /// What a capture read: a pane's (and then its state's fields), or a
    /// client's.
    CapturedField {
        Pane "pane",
        Version "version",
        WrapPending "wrap_pending",
        Client "client",
    }
);

/// Project held native observations directly; never resolve their source again.
pub(super) fn answer_field(
    answer: &super::Answer,
    run: Option<Run>,
    field: &str,
) -> Result<V, Failure> {
    use fux_wire::answer::{Ended, Of, PaneAnswer};
    let b = &mut Builder;
    let absent = || {
        Failure::new(
            Code::from(Raise::NoField).into(),
            format!("held observation has no field {field:?}"),
        )
    };
    match answer {
        super::Answer::Pane(answer) => {
            let field = AnswerField::named(field).ok_or_else(absent)?;
            let (changed, ended) = match answer {
                PaneAnswer::Changed { changed, ended, .. } => (Some(changed), *ended),
                PaneAnswer::Acted(_) | PaneAnswer::Closed(_) | PaneAnswer::TimedOut { .. } => {
                    (None, None)
                }
            };
            match field {
                AnswerField::Pane => Ok(Value::App(Fux::Pane(answer.pane(), run))),
                AnswerField::Version => answer
                    .version()
                    .map(|v| b.text(&v.to_string()))
                    .unwrap_or_else(|| Err(absent())),
                AnswerField::Status => number(answer.failure().map_or(0, |c| c.status().get())),
                AnswerField::Changed => {
                    let changed = changed.ok_or_else(absent)?;
                    b.list(changed, |b, c| b.text(c.name()))
                }
                AnswerField::Exited | AnswerField::Signal => {
                    changed.ok_or_else(absent)?;
                    match (field, ended) {
                        (AnswerField::Exited, Some(Ended::Exited(n)))
                        | (AnswerField::Signal, Some(Ended::Signalled(n))) => number(n),
                        (_, Some(_)) => Ok(null()),
                        (_, None) => Err(absent()),
                    }
                }
            }
        }
        super::Answer::Capture(c) => {
            if let Some(own) = CaptureField::named(field) {
                return capture_field(b, c, own).unwrap_or_else(|| Err(absent()));
            }
            let named = CapturedField::named(field);
            match (&c.of, named) {
                (Of::Pane { version, .. }, Some(CapturedField::Pane)) => {
                    Ok(Value::App(Fux::Pane(version.pane, Some(version.run))))
                }
                (Of::Pane { version, .. }, Some(CapturedField::Version)) => {
                    b.text(&version.to_string())
                }
                (Of::Pane { wrap_pending, .. }, Some(CapturedField::WrapPending)) => {
                    Ok(Value::Bool(*wrap_pending))
                }
                (Of::Pane { state, .. }, None) => state
                    .iter()
                    .find(|(name, _)| name == field)
                    .map(|(_, v)| b.json(v, 0))
                    .unwrap_or_else(|| Err(absent())),
                (Of::Client(client), Some(CapturedField::Client)) => {
                    Ok(Value::App(Fux::Client(*client, run)))
                }
                (Of::Pane { .. }, Some(CapturedField::Client))
                | (
                    Of::Client(_),
                    None
                    | Some(CapturedField::Pane | CapturedField::Version | CapturedField::WrapPending),
                ) => Err(absent()),
            }
        }
        super::Answer::Panes(_) => Err(absent()),
    }
}

/// A capture's own field, if it has it: `styles` only if read with them.
fn capture_field(
    b: &mut Builder,
    c: &fux_wire::answer::Captured,
    field: CaptureField,
) -> Option<Result<V, Failure>> {
    Some(match field {
        CaptureField::ScreenRows => number(c.screen_rows),
        CaptureField::ScreenCols => number(c.screen_cols),
        CaptureField::ScreenCursor => match c.screen_cursor {
            Some((row, col)) => b.list(&[row, col], |_, n| number(*n)),
            None => Ok(null()),
        },
        CaptureField::Lines => b.list(&c.lines, |b, line| {
            let mut text = String::new();
            // Writing to a `String` can't fail.
            _ = c.write_line(&mut text, line, false, &mut Vec::new());
            b.text(&text)
        }),
        CaptureField::Styles if !c.styled => return None,
        CaptureField::Styles => b.list(&c.lines, |b, line| styles(b, c, line)),
    })
}

/// A captured line's styled runs, a record each: where it starts and how
/// long it is, then what its style sets.
fn styles(
    b: &mut Builder,
    c: &fux_wire::answer::Captured,
    line: &fux_wire::answer::Line,
) -> Result<V, Failure> {
    let styled = |run: &Option<(&str, u32, fux_vt::Attributes)>| {
        run.is_some_and(|(_, _, style)| style != fux_vt::Attributes::default())
    };
    let mut values = Vec::with_capacity(c.runs(line).filter(styled).count());
    let mut col = 0u32;
    for run in c.runs(line) {
        let (_, cells, style) = run.ok_or_else(fail)?;
        if style != fux_vt::Attributes::default() {
            let mut fields = Vec::with_capacity(2);
            for (name, value) in [("col", number(col)?), ("len", number(cells)?)] {
                fields.push((name.to_owned(), value));
            }
            for (name, value) in fux_wire::answer::style_fields(style) {
                let value = match value {
                    fux_wire::answer::StyleValue::Flag => Value::Bool(true),
                    fux_wire::answer::StyleValue::Index(n) => number(n)?,
                    fux_wire::answer::StyleValue::Rgb(fux_vt::Rgb { r, g, b: blue }) => {
                        b.text(&format!("#{r:02x}{g:02x}{blue:02x}"))?
                    }
                };
                fields.push((name.to_owned(), value));
            }
            values.push(b.finish(fields, None)?);
        }
        col = col.checked_add(cells).ok_or_else(fail)?;
    }
    Ok(Value::List(Rc::new(
        List::new(values).ok_or_else(fail)?,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maximum_event_record_shape_fits_its_construction_allowance() -> Result<(), Failure> {
        use fux_wire::events::{Actor, Batch, Cursor, Item, Status};
        use fux_wire::id::{ClientId, PaneId, TabId, WsId};
        let next: Cursor = "18446744073709551615:4294967295-ffffffffffffffff"
            .parse()
            .map_err(|()| fail())?;
        let item = Item {
            // Combine the independent maxima, even fields no single variant
            // needs together: 12 item fields, 3 origin fields and all strings.
            data: Json::object(vec![
                (
                    "event",
                    Json::str(std::iter::repeat_n('x', 32).collect::<String>()),
                ),
                (
                    "text",
                    Json::str(
                        std::iter::repeat_n('𐀀', fux_server::pane::NOTIFICATION_MAX)
                            .collect::<String>(),
                    ),
                ),
                ("truncated", Json::Bool(true)),
                ("input_unread", Json::Int(i64::MAX)),
            ]),
            pane: Some(PaneId::of(u32::MAX)),
            tab: Some(TabId::of(u32::MAX)),
            workspace: Some(WsId::of(u32::MAX)),
            client: Some(ClientId::of(u32::MAX)),
            moved_from: Some((WsId::of(u32::MAX), TabId::of(u32::MAX))),
            origin: Actor::Program(PaneId::of(u32::MAX), u64::MAX / 2),
            next,
        };
        for count in [0, 1, fux_server::journal::BATCH_EVENTS] {
            for reason in [
                None,
                Some(std::iter::repeat_n('𐀀', 500).collect::<String>()),
            ] {
                let bytes = event_bytes(count, reason.as_deref());
                let batch = Batch {
                    events: vec![item.clone(); count],
                    next,
                    missed: 0,
                    status: reason.map_or(Status::Continuing, Status::Stopped),
                };
                let grant = Imports::new(bytes);
                let value = events(batch, &grant)?;
                assert_eq!(grant.remaining(), 0);
                value.retain(&grant)?;
                assert!(grant.remaining() > 0);
                drop(value);
                assert_eq!(grant.remaining(), bytes);
            }
        }
        Ok(())
    }

    #[test]
    fn event_records_reserve_before_building_and_keep_typed_identities() -> Result<(), Failure> {
        use fux_wire::events::{Actor, Batch, Cursor, Item, Status};
        use fux_wire::id::{ClientId, PaneId, TabId, WsId};
        let next: Cursor = "1:1-0".parse().map_err(|()| fail())?;
        for origin in [
            Actor::Unknown,
            Actor::Server,
            Actor::Client(ClientId::of(3)),
            Actor::Pane(PaneId::of(4), 5),
            Actor::Program(PaneId::of(4), 5),
        ] {
            let batch = Batch {
                next,
                missed: 0,
                status: Status::Continuing,
                events: vec![Item {
                    data: Json::object(vec![
                        ("event", Json::str("moved")),
                        (
                            "name",
                            Json::str(std::iter::repeat_n('𐀀', 64).collect::<String>()),
                        ),
                    ]),
                    pane: Some(PaneId::of(1)),
                    tab: Some(TabId::of(2)),
                    workspace: Some(WsId::of(3)),
                    client: Some(ClientId::of(4)),
                    moved_from: Some((WsId::of(5), TabId::of(6))),
                    origin,
                    next,
                }],
            };
            let grant = Imports::new(32 << 10);
            let root = events(batch.clone(), &grant)?;
            let reserved = event_bytes(1, None);
            assert_eq!(grant.remaining(), (32 << 10) - reserved);
            root.retain(&grant)?;
            let exact_bytes = (32 << 10) - grant.remaining();
            assert!(
                exact_bytes < reserved,
                "admission trims the construction allowance"
            );
            let Value::App(Fux::Record(record)) = &root else {
                return Err(fail());
            };
            let Value::List(list) = record.get("events")? else {
                return Err(fail());
            };
            let Value::App(Fux::Record(event)) = list.items().first().ok_or_else(fail)? else {
                return Err(fail());
            };
            assert_eq!(
                event
                    .fields
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
                [
                    "event",
                    "pane",
                    "tab",
                    "workspace",
                    "client",
                    "from_tab",
                    "from_workspace",
                    "name",
                    "origin",
                    "next"
                ]
            );
            assert_eq!(
                event.get("pane")?,
                id(AnyRef::Pane(PaneId::of(1)), next.run)
            );
            assert_eq!(
                event.get("from_tab")?,
                id(AnyRef::Tab(TabId::of(6)), next.run)
            );
            assert_eq!(
                event.get("client")?,
                Value::App(Fux::Client(ClientId::of(4), Some(next.run)))
            );
            let Value::App(Fux::Record(actor)) = event.get("origin")? else {
                return Err(fail());
            };
            assert_eq!(actor.get("kind")?, Value::str(origin.kind()));
            match origin {
                Actor::Client(c) => assert_eq!(
                    actor.get("client")?,
                    Value::App(Fux::Client(c, Some(next.run)))
                ),
                Actor::Pane(p, generation) | Actor::Program(p, generation) => {
                    assert_eq!(actor.get("pane")?, id(AnyRef::Pane(p), next.run));
                    assert_eq!(actor.get("generation")?, number(generation)?);
                }
                Actor::Unknown | Actor::Server => {}
            }
            let full = reserved;
            let small = Imports::new(full - 1);
            assert!(events(batch.clone(), &small).is_err());
            assert_eq!(
                small.remaining(),
                full - 1,
                "failed builds refund their leases"
            );
            let exact = Imports::new(full);
            for _ in 0..100 {
                let held = events(batch.clone(), &exact)?;
                assert_eq!(
                    exact.remaining(),
                    0,
                    "the whole allowance stays held until admission"
                );
                held.retain(&exact)?;
                assert_eq!(exact.remaining(), full - exact_bytes);
                held.clone().retain(&exact)?;
                assert_eq!(
                    exact.remaining(),
                    full - exact_bytes,
                    "aliases do not charge twice"
                );
                drop(held);
                assert_eq!(exact.remaining(), full);
            }
            let child = event.get("name")?;
            drop(actor);
            drop(list);
            drop(root);
            assert!(grant.remaining() < 32 << 10);
            child.retain(&grant)?;
            drop(child);
            assert_eq!(grant.remaining(), 32 << 10);
        }
        Ok(())
    }

    #[test]
    fn local_fields_preserve_null_lists_and_scalar_types() -> Result<(), Failure> {
        let value = from_json(&Json::Object(vec![
            ("empty".into(), Json::Null),
            (
                "items".into(),
                Json::Array(vec![
                    Json::String("%7".into()),
                    Json::Bool(false),
                    Json::Int(-9),
                ]),
            ),
        ]))?;
        let Value::App(Fux::Record(r)) = value else {
            return Err(fail());
        };
        assert_eq!(r.get("empty")?, null());
        assert_eq!(
            r.get("missing").map_err(|e| e.code),
            Err(Code::from(Raise::NoField).into())
        );
        let Value::List(items) = r.get("items")? else {
            return Err(fail());
        };
        assert_eq!(
            items.items(),
            &[Value::str("%7"), Value::Bool(false), Value::Int(-9)]
        );
        assert_eq!(
            from_json(&Json::Uint(u64::MAX)).map_err(|e| e.code),
            Err(fuxth::Code::Range)
        );
        assert_eq!(r.get("items")?, Value::List(items));
        Ok(())
    }

    #[test]
    fn projected_children_keep_only_their_shared_storage_reserved() -> Result<(), Failure> {
        let grant = fuxth::Imports::new(2048);
        let make = || {
            from_json(&Json::Object(vec![(
                "text".into(),
                Json::String(std::iter::repeat_n('x', 1000).collect()),
            )]))
        };
        let root = make()?;
        root.retain(&grant)?;
        let Value::App(Fux::Record(record)) = &root else {
            return Err(fail());
        };
        let child = record.get("text")?;
        child.retain(&grant)?;
        for _ in 0..10 {
            root.clone().retain(&grant)?;
            child.clone().retain(&grant)?;
        }
        assert!(make()?.retain(&grant).is_err());
        drop(root);
        assert!(make()?.retain(&grant).is_err());
        drop(child);
        for _ in 0..100 {
            make()?.retain(&grant)?;
        }
        Ok(())
    }

    #[test]
    fn records_refuse_depth_nodes_and_bytes_before_retention() {
        let mut deep = Json::Null;
        for _ in 0..inspect::DEPTH {
            deep = Json::Array(vec![deep]);
        }
        assert!(from_json(&deep).is_err());
        assert!(from_json(&Json::Array(vec![Json::Bool(false); inspect::NODES])).is_err());
        let text = std::iter::repeat_n('a', fuxth::LONGEST).collect::<String>();
        assert!(from_json(&Json::Array(vec![Json::String(text); 9])).is_err());
        let mut budget = Budget::with_limit(inspect::BYTES);
        let mut b = Builder::new(&mut budget);
        assert!(
            b.text(&std::iter::repeat_n('a', fuxth::LONGEST.saturating_add(1)).collect::<String>())
                .is_err()
        );
    }
}
