//! Immutable local projections. Only checked constructors can retain a record.
use super::{Fux, handle};
use crate::code::{Code, Raise};
use crate::command::{AnyRef, Run};
use crate::inspect::{self, Projection, Scope};
use crate::json::Json;
use crate::wire::Budget;
use fuxix::Allocation;
use fuxth::{AppValue, Failure, ImportLease, Imports, List, Shared, Text, Value};

type V = Value<Fux>;

#[derive(Clone, Debug)]
pub struct Record {
    pub(super) fields: Vec<(String, V)>,
    weight: u64,
    depth: u8,
    bytes: usize,
    pub(super) of: Option<Run>,
}

impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}

impl Record {
    pub(super) fn retain(
        this: &fuxth::Shared<Self>,
        imports: &fuxth::Imports,
    ) -> Result<(), Failure> {
        let own = Self::own_bytes(&this.fields);
        this.reserve(imports, own)?;
        for (_, value) in &this.fields {
            value.retain(imports)?;
        }
        Ok(())
    }

    fn own_bytes(fields: &Vec<(String, V)>) -> usize {
        fields.iter().fold(
            fuxth::Shared::<Record>::bytes(0).saturating_add(
                fields
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(String, V)>()),
            ),
            |n, (key, _)| n.saturating_add(key.capacity()),
        )
    }

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
    pub(super) fn weight(&self) -> u64 {
        self.weight
    }
    pub(super) fn depth(&self) -> u8 {
        self.depth
    }
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(super) fn write(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        out.write_char('{')?;
        for (i, (key, value)) in self.fields.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            crate::json::write_str(out, key)?;
            out.write_char(':')?;
            super::words::write_json_value(value, out)?;
        }
        out.write_char('}')
    }
}

fn fail() -> Failure {
    Failure::new(
        fuxth::Code::TooLong,
        "record exceeds its depth, node, byte or language limit; request a smaller scope",
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

struct Builder<'a> {
    budget: &'a mut Budget,
    nodes: usize,
    lease: ImportLease,
}
impl<'a> Builder<'a> {
    fn new(budget: &'a mut Budget) -> Self {
        Self {
            budget,
            nodes: 0,
            lease: ImportLease::default(),
        }
    }
    fn nodes(&mut self, n: usize) -> Result<(), Failure> {
        self.nodes = self
            .nodes
            .checked_add(n)
            .filter(|n| *n <= inspect::NODES)
            .ok_or_else(fail)?;
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<V, Failure> {
        if text.chars().count() > fuxth::LONGEST {
            return Err(fail());
        }
        self.budget.text_value(text)?;
        Ok(Value::str(self.budget.string(text)?))
    }
    fn optional(&mut self, text: Option<&str>) -> Result<V, Failure> {
        text.map(|s| self.text(s)).unwrap_or_else(|| Ok(null()))
    }
    fn list<T>(
        &mut self,
        items: &[T],
        mut value: impl FnMut(&mut Self, &T) -> Result<V, Failure>,
    ) -> Result<V, Failure> {
        self.nodes(items.len())?;
        let mut out = self.budget.vector(items.len())?;
        for item in items {
            out.push(value(self, item)?);
        }
        self.budget.charge(fuxth::Shared::<List<Fux>>::bytes(0))?;
        let list = List::new(out).ok_or_else(fail)?;
        Ok(Value::List(fuxth::Shared::new(list)))
    }
    fn record<const N: usize>(&mut self, fields: [(&str, V); N]) -> Result<V, Failure> {
        self.nodes(N.saturating_add(1))?;
        let mut out = self.budget.vector(N)?;
        for (key, value) in fields {
            out.push((self.budget.string(key)?, value));
        }
        self.finish(out, None)
    }
    fn finish(&mut self, fields: Vec<(String, V)>, of: Option<Run>) -> Result<V, Failure> {
        let (mut depth, mut weight, mut bytes) = (0u8, 1u64, Record::own_bytes(&fields));
        for (key, value) in &fields {
            depth = depth.max(value.depth());
            weight = weight
                .checked_add(key.len() as u64)
                .and_then(|n| n.checked_add(value.weight()))
                .ok_or_else(fail)?;
            bytes = bytes.saturating_add(
                value
                    .storage_bytes(AppValue::text_bytes)
                    .unwrap_or(usize::MAX),
            );
        }
        let depth = depth
            .checked_add(1)
            .filter(|d| *d <= inspect::DEPTH)
            .ok_or_else(fail)?;
        if weight > fuxth::TOTAL {
            return Err(fail());
        }
        self.budget.charge(fuxth::Shared::<Record>::bytes(0))?;
        Ok(Value::App(Fux::Record(Shared::leased(
            Record {
                fields,
                depth,
                weight,
                bytes,
                of,
            },
            std::mem::take(&mut self.lease),
        ))))
    }
    fn json(&mut self, value: &Json, depth: u8) -> Result<V, Failure> {
        if depth >= inspect::DEPTH {
            return Err(fail());
        }
        self.nodes(1)?;
        match value {
            Json::Null => Ok(null()),
            Json::Bool(b) => Ok(Value::Bool(*b)),
            Json::Int(n) => Ok(Value::Int(*n)),
            Json::Uint(n) => number(*n),
            Json::String(s) => self.text(s),
            Json::Array(items) => self.list(items, |b, v| b.json(v, depth.saturating_add(1))),
            Json::Object(fields) => {
                let mut out = self.budget.vector(fields.len())?;
                for (key, value) in fields {
                    let key = self.budget.string(key)?;
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
                let mut text = String::new();
                for byte in bytes {
                    self.budget.write(&mut text, format_args!("{byte:02x}"))?;
                }
                ("hex", self.text(&text)?)
            }
        };
        let encoding = self.text(encoding)?;
        self.record([("encoding", encoding), ("text", text)])
    }
}

pub(super) fn from_fields(
    fields: Vec<(String, V)>,
    of: Option<Run>,
    budget: &mut Budget,
) -> Result<V, Failure> {
    let mut b = Builder::new(budget);
    b.nodes(fields.len())?;
    b.finish(fields, of)
}

pub(super) fn from_json(value: &Json) -> Result<V, Failure> {
    Builder::new(&mut Budget::with_limit(inspect::BYTES)).json(value, 0)
}

// Per event: one list slot, two records (at most 12 item fields and 3
// origin fields), their keys, and four strings (up to 2,000 payload bytes).
// 8 KiB covers these; 1 KiB covers the list/outer-record headers, five outer
// fields, cursor/status and temporary cursor formatting. The maximum-shape
// test checks this allowance; a stopped reason is reserved separately.
fn event_bytes(events: usize, reason: Option<&str>) -> usize {
    1024usize
        .saturating_add(reason.map_or(0, |r| Text::size(r.len(), 0)))
        .saturating_add(8192usize.saturating_mul(events))
}

pub(super) fn events(batch: crate::events::Batch, imports: &Imports) -> Result<V, Failure> {
    use crate::events::{Actor, Status};
    let run = batch.next.run;
    let reason = match &batch.status {
        Status::Stopped(reason) => Some(reason.as_str()),
        Status::Continuing | Status::Timeout | Status::Gone => None,
    };
    let bytes = event_bytes(batch.events.len(), reason);
    let lease = imports.lease(bytes)?;
    // This builder spends only the allowance already held in the run.
    let mut budget = Budget::with_limit(bytes);
    let mut b = Builder::new(&mut budget);
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
        b.nodes(count)?;
        let mut out = b.budget.vector(count)?;
        // The producer puts the event name first; IDs remain typed throughout.
        for (key, value) in data.iter().take(1) {
            out.push((b.budget.string(key)?, b.json(value, 0)?));
        }
        for (key, value) in ids {
            if let Some(value) = value {
                out.push((b.budget.string(key)?, value));
            }
        }
        for (key, value) in data.iter().skip(1) {
            out.push((b.budget.string(key)?, b.json(value, 0)?));
        }
        out.push((b.budget.string("origin")?, origin));
        out.push((b.budget.string("next")?, next));
        b.finish(out, None)
    })?;
    let next = b.text(&batch.next.to_string())?;
    let status = b.text(batch.status.name())?;
    let reason = b.optional(reason)?;
    // The outer record holds the construction reservation until admission.
    // retain then refunds the excess and gives each child its own lease.
    b.lease = lease;
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
    let budget = &mut Budget::with_limit(inspect::BYTES);
    let b = &mut Builder::new(budget);
    let run = s.run;
    let workspaces = b.list(&s.workspaces, |b,w| record! { b;
        "id" => id(AnyRef::Workspace(w.id), run), "name" => b.optional(w.name.as_deref())?,
        "label" => b.text(&w.label)?, "tabs" => b.list(&w.tabs, |_,t| Ok(id(AnyRef::Tab(*t), run)))?
    })?;
    let tabs = b.list(&s.tabs, |b,t| {
        let nodes = b.list(&t.nodes, |b,n| match n {
            crate::layout::Node::Pane(p) => Ok(id(AnyRef::Pane(*p), run)),
            crate::layout::Node::Split { axis, first, second, first_share, second_share } => {
                let row = [b.text(match axis { crate::layout::Axis::Horizontal => "horizontal", crate::layout::Axis::Vertical => "vertical" })?, number(first_share.get())?, number(second_share.get())?, number(*first)?, number(*second)?];
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
        let mut fields = b.budget.vector(base.len().saturating_add(2))?;
        for (key, value) in base { fields.push((b.budget.string(key)?, value)); }
        if let Some(state) = &p.state { fields.push((b.budget.string("state")?, b.json(state, 0)?)); }
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
            fields.push((b.budget.string("launch")?, recipe));
        }
        b.nodes(fields.len())?;
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
    fuxth::Shared::get_mut(&mut record).ok_or_else(fail)?.of = Some(run);
    Ok(Value::App(Fux::Record(record)))
}

/// Project held native observations directly; never resolve their source again.
pub(super) fn answer_field(
    answer: &super::Answer,
    run: Option<Run>,
    field: &str,
) -> Result<V, Failure> {
    use crate::answer::PaneAnswer;
    let mut budget = Budget::with_limit(inspect::BYTES);
    let b = &mut Builder::new(&mut budget);
    let absent = || {
        Failure::new(
            Code::from(Raise::NoField).into(),
            format!("held observation has no field {field:?}"),
        )
    };
    match answer {
        super::Answer::Pane(answer) => {
            match field {
                "pane" => return Ok(Value::App(Fux::Pane(answer.pane(), run))),
                "version" => {
                    return answer
                        .version()
                        .map(|v| b.text(&v.to_string()))
                        .unwrap_or_else(|| Err(absent()));
                }
                "status" => return number(answer.failure().map_or(0, |c| c.status().get())),
                _ => {}
            }
            if let PaneAnswer::Changed { changed, ended, .. } = answer {
                match (field, ended) {
                    ("changed", _) => return b.list(changed, |b, c| b.text(c.name())),
                    ("exited", Some(crate::process::Ended::Exited(n))) => return number(*n),
                    ("signal", Some(crate::process::Ended::Signalled(n))) => return number(*n),
                    ("exited" | "signal", Some(_)) => return Ok(null()),
                    _ => {}
                }
            }
            Err(absent())
        }
        super::Answer::Capture(c) => {
            match field {
                "screen_rows" => return number(c.screen_rows),
                "screen_cols" => return number(c.screen_cols),
                "screen_cursor" => {
                    return match c.screen_cursor {
                        Some((row, col)) => b.list(&[row, col], |_, n| number(*n)),
                        None => Ok(null()),
                    };
                }
                "lines" => {
                    return b.list(&c.lines, |b, line| {
                        let text = crate::json::render(b.budget, |out| {
                            c.write_line(out, line, false, &mut Vec::new())
                        })?;
                        b.text(&text)
                    });
                }
                "styles" if c.styled => {
                    return b.list(&c.lines, |b, line| {
                        let styled = |run: &Option<(&str, u32, fux_vt::Attributes)>| {
                            run.is_some_and(|(_, _, style)| style != fux_vt::Attributes::default())
                        };
                        let mut values = b.budget.vector(c.runs(line).filter(styled).count())?;
                        let mut col = 0u32;
                        for run in c.runs(line) {
                            let (_, cells, style) = run.ok_or_else(fail)?;
                            if style != fux_vt::Attributes::default() {
                                let mut fields = b.budget.vector(2)?;
                                for (name, value) in
                                    [("col", number(col)?), ("len", number(cells)?)]
                                {
                                    fields.push((b.budget.string(name)?, value));
                                }
                                for (name, value) in crate::capture::style_fields(style) {
                                    let value = match value {
                                        crate::capture::StyleValue::Flag => Value::Bool(true),
                                        crate::capture::StyleValue::Index(n) => number(n)?,
                                        crate::capture::StyleValue::Rgb(fux_vt::Rgb {
                                            r,
                                            g,
                                            b: blue,
                                        }) => b.text(&format!("#{r:02x}{g:02x}{blue:02x}"))?,
                                    };
                                    b.budget.reserve(&mut fields, 1)?;
                                    fields.push((b.budget.string(name)?, value));
                                }
                                b.nodes(fields.len().saturating_add(1))?;
                                values.push(b.finish(fields, None)?);
                            }
                            col = col.checked_add(cells).ok_or_else(fail)?;
                        }
                        b.nodes(values.len())?;
                        b.budget.charge(fuxth::Shared::<List<Fux>>::bytes(0))?;
                        Ok(Value::List(fuxth::Shared::new(
                            List::new(values).ok_or_else(fail)?,
                        )))
                    });
                }
                _ => {}
            }
            match &c.of {
                crate::capture::Of::Pane {
                    version,
                    wrap_pending,
                    state,
                } => match field {
                    "pane" => Ok(Value::App(Fux::Pane(version.pane, Some(version.run)))),
                    "version" => b.text(&version.to_string()),
                    "wrap_pending" => Ok(Value::Bool(*wrap_pending)),
                    field => state
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, v)| b.json(v, 0))
                        .unwrap_or_else(|| Err(absent())),
                },
                crate::capture::Of::Client(client) => {
                    if field == "client" {
                        Ok(Value::App(Fux::Client(*client, run)))
                    } else {
                        Err(absent())
                    }
                }
            }
        }
        super::Answer::Panes(_) => Err(absent()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maximum_event_record_shape_fits_its_construction_allowance() -> Result<(), Failure> {
        use crate::events::{Actor, Batch, Cursor, Item, Status};
        use crate::id::{ClientId, PaneId, TabId, WsId};
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
                        std::iter::repeat_n('𐀀', crate::pane::NOTIFICATION_MAX).collect::<String>(),
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
        for count in [0, 1, crate::events::BATCH_EVENTS] {
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
        use crate::events::{Actor, Batch, Cursor, Item, Status};
        use crate::id::{ClientId, PaneId, TabId, WsId};
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
