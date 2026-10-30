//! Immutable local projections. Only checked constructors can retain a record.
use super::{Fux, handle};
use crate::code::{Code, Raise};
use crate::command::{AnyRef, Run};
use crate::inspect::{self, Projection, Scope};
use crate::json::Json;
use fuxth::{Failure, Grant, Items, Reservation, Value};
use std::rc::Rc;

type V = Value<Fux>;

#[derive(Clone, Debug)]
pub struct Record {
    pub(super) fields: Vec<(String, V)>,
    weight: u64,
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
    pub(super) fn weight(&self) -> u64 {
        self.weight
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
            crate::json::write_str(out, key)?;
            out.write_char(':')?;
            super::words::write_json_value(value, out)?;
        }
        out.write_char('}')
    }
}

/// A record's fields being made: their room, and their keys', reserved
/// before each is allocated.
pub(super) struct Fields {
    fields: Vec<(String, V)>,
    held: Reservation,
}

impl Fields {
    /// Room for `n` fields, reserved from `grant` first.
    pub(super) fn new(grant: &Rc<Grant>, n: usize) -> Result<Self, Failure> {
        let room = n.checked_mul(std::mem::size_of::<(String, V)>());
        let room = room.ok_or_else(|| grant.full())?;
        let held = Reservation::take(grant, fuxth::Shared::<Record>::bytes(room))?;
        let mut fields = Vec::new();
        fields.try_reserve_exact(n).map_err(|_| grant.full())?;
        Ok(Self { fields, held })
    }

    /// Field `key` added, its room and its key's reserved first.
    pub(super) fn push(&mut self, key: &str, value: V) -> Result<(), Failure> {
        self.held.grow(&mut self.fields, 1)?;
        self.held
            .resize(self.held.bytes().saturating_add(key.len()))?;
        let mut owned = String::new();
        owned.try_reserve_exact(key.len()).map_err(|_| fail())?;
        owned.push_str(key);
        self.fields.push((owned, value));
        Ok(())
    }

    fn len(&self) -> usize {
        self.fields.len()
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
    grant: &'a Rc<Grant>,
    nodes: usize,
}
impl<'a> Builder<'a> {
    fn new(grant: &'a Rc<Grant>) -> Self {
        Self { grant, nodes: 0 }
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
        Value::str(self.grant, text)
    }
    /// A string of what `write` writes, reserved before it is made.
    fn written(
        &mut self,
        mut write: impl FnMut(&mut dyn std::fmt::Write) -> std::fmt::Result,
    ) -> Result<V, Failure> {
        let (text, held) = fuxth::written(self.grant, |out| write(out).map_err(|_| fail()))?;
        fuxth::Text::adopt(text, held).map(Value::Str)
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
        let mut out = Items::new(self.grant, items.len())?;
        for item in items {
            out.push(value(self, item)?)?;
        }
        out.finish().ok_or_else(fail)
    }
    fn fields(&mut self, n: usize) -> Result<Fields, Failure> {
        Fields::new(self.grant, n)
    }
    fn record<const N: usize>(&mut self, fields: [(&str, V); N]) -> Result<V, Failure> {
        self.nodes(N.saturating_add(1))?;
        let mut out = self.fields(N)?;
        for (key, value) in fields {
            out.push(key, value)?;
        }
        self.finish(out, None)
    }
    fn finish(&mut self, fields: Fields, of: Option<Run>) -> Result<V, Failure> {
        let (mut depth, mut weight) = (0u8, 1u64);
        for (key, value) in &fields.fields {
            depth = depth.max(value.depth());
            weight = weight
                .checked_add(key.len() as u64)
                .and_then(|n| n.checked_add(value.weight()))
                .ok_or_else(fail)?;
        }
        let depth = depth
            .checked_add(1)
            .filter(|d| *d <= inspect::DEPTH)
            .ok_or_else(fail)?;
        if weight > fuxth::TOTAL {
            return Err(fail());
        }
        let record = Record {
            fields: fields.fields,
            depth,
            weight,
            of,
        };
        Ok(Value::App(Fux::Record(fuxth::Shared::new(
            record,
            fields.held,
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
                let mut out = self.fields(fields.len())?;
                for (key, value) in fields {
                    let value = self.json(value, depth.saturating_add(1))?;
                    out.push(key, value)?;
                }
                self.finish(out, None)
            }
        }
    }
    fn path(&mut self, path: &inspect::Path) -> Result<V, Failure> {
        let (encoding, text) = match path {
            inspect::Path::Utf8(text) => ("utf8", self.text(text)?),
            inspect::Path::Bytes(bytes) => {
                let hex = self
                    .written(|out| bytes.iter().try_for_each(|byte| write!(out, "{byte:02x}")))?;
                ("hex", hex)
            }
        };
        let encoding = self.text(encoding)?;
        self.record([("encoding", encoding), ("text", text)])
    }
}

pub(super) fn from_fields(
    fields: Fields,
    of: Option<Run>,
    grant: &Rc<Grant>,
) -> Result<V, Failure> {
    let mut b = Builder::new(grant);
    b.nodes(fields.len())?;
    b.finish(fields, of)
}

pub(super) fn from_json(value: &Json, grant: &Rc<Grant>) -> Result<V, Failure> {
    Builder::new(grant).json(value, 0)
}

pub(super) fn events(batch: crate::events::Batch, grant: &Rc<Grant>) -> Result<V, Failure> {
    let run = batch.next.run;
    if batch.events.len() > crate::events::BATCH_EVENTS {
        return Err(fail());
    }
    let mut b = Builder::new(grant);
    let mut previous = None;
    let events = b.list(&batch.events, |b, item| {
        if item.next.run != run
            || item.next.seq > batch.next.seq
            || previous.is_some_and(|seq| item.next.seq <= seq)
        {
            return Err(fail());
        }
        previous = Some(item.next.seq);
        if !matches!(item.data.get("event"), Some(Json::String(_))) {
            return Err(fail());
        }
        let json = item.json();
        let Json::Object(fields) = &json else {
            return Err(fail());
        };
        let mut out = b.fields(fields.len().saturating_add(2))?;
        for (key, value) in fields
            .iter()
            .filter(|(key, _)| key != "origin" && key != "next")
        {
            b.nodes(1)?;
            let native = match key.as_str() {
                "pane" => item.pane.map(|p| id(AnyRef::Pane(p), run)),
                "tab" => item.tab.map(|t| id(AnyRef::Tab(t), run)),
                "workspace" => item.workspace.map(|w| id(AnyRef::Workspace(w), run)),
                "client" => item.client.map(|c| Value::App(Fux::Client(c, Some(run)))),
                "from_tab" => item.moved_from.map(|(_, t)| id(AnyRef::Tab(t), run)),
                "from_workspace" => item.moved_from.map(|(w, _)| id(AnyRef::Workspace(w), run)),
                _ => None,
            };
            let value = match native {
                Some(value) => value,
                None => b.json(value, 0)?,
            };
            out.push(key, value)?;
        }
        use crate::events::Actor;
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
        b.nodes(2)?;
        out.push("origin", origin)?;
        let next = b.text(&item.next.to_string())?;
        out.push("next", next)?;
        b.finish(out, None)
    })?;
    let next = b.text(&batch.next.to_string())?;
    let status = b.text(batch.status.name())?;
    let reason = match &batch.status {
        crate::events::Status::Stopped(reason) => b.text(reason)?,
        crate::events::Status::Continuing
        | crate::events::Status::Timeout
        | crate::events::Status::Gone => null(),
    };
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

pub(super) fn snapshot(s: inspect::Snapshot, grant: &Rc<Grant>) -> Result<V, Failure> {
    let b = &mut Builder::new(grant);
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
        let mut fields = b.fields(base.len().saturating_add(2))?;
        for (key, value) in base { fields.push(key, value)?; }
        if let Some(state) = &p.state { let state = b.json(state, 0)?; fields.push("state", state)?; }
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
            fields.push("launch", recipe)?;
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
    fuxth::Shared::get_mut(&mut record).ok_or_else(fail)?.0.of = Some(run);
    Ok(Value::App(Fux::Record(record)))
}

/// Project held native observations directly; never resolve their source again.
pub(super) fn answer_field(
    answer: &super::Answer,
    run: Option<Run>,
    field: &str,
    grant: &Rc<Grant>,
) -> Result<V, Failure> {
    use crate::answer::PaneAnswer;
    let b = &mut Builder::new(grant);
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
                        b.written(|out| c.write_line(out, line, false, &mut Vec::new()))
                    });
                }
                "styles" if c.styled => {
                    return b.list(&c.lines, |b, line| {
                        let styled = |run: &Option<(&str, u32, fux_vt::Attributes)>| {
                            run.is_some_and(|(_, _, style)| style != fux_vt::Attributes::default())
                        };
                        let n = c.runs(line).filter(styled).count();
                        b.nodes(n)?;
                        let mut values = Items::new(b.grant, n)?;
                        let mut col = 0u32;
                        for run in c.runs(line) {
                            let (_, cells, style) = run.ok_or_else(fail)?;
                            if style != fux_vt::Attributes::default() {
                                let mut fields = b.fields(2)?;
                                fields.push("col", number(col)?)?;
                                fields.push("len", number(cells)?)?;
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
                                    fields.push(name, value)?;
                                }
                                b.nodes(fields.len().saturating_add(1))?;
                                values.push(b.finish(fields, None)?)?;
                            }
                            col = col.checked_add(cells).ok_or_else(fail)?;
                        }
                        values.finish().ok_or_else(fail)
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

    fn grant(bytes: usize) -> Rc<Grant> {
        Rc::new(Grant::new(bytes))
    }

    #[test]
    fn local_fields_preserve_null_lists_and_scalar_types() -> Result<(), Failure> {
        let grant = grant(1 << 20);
        let value = from_json(
            &Json::Object(vec![
                ("empty".into(), Json::Null),
                (
                    "items".into(),
                    Json::Array(vec![
                        Json::String("%7".into()),
                        Json::Bool(false),
                        Json::Int(-9),
                    ]),
                ),
            ]),
            &grant,
        )?;
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
            &[
                Value::str(&grant, "%7")?,
                Value::Bool(false),
                Value::Int(-9)
            ]
        );
        assert_eq!(
            from_json(&Json::Uint(u64::MAX), &grant).map_err(|e| e.code),
            Err(fuxth::Code::Range)
        );
        assert_eq!(r.get("items")?, Value::List(items));
        Ok(())
    }

    #[test]
    fn projected_children_keep_only_their_shared_storage_reserved() -> Result<(), Failure> {
        let grant = grant(2048);
        let make = || {
            let text = Json::String(std::iter::repeat_n('x', 1000).collect());
            from_json(&Json::Object(vec![("text".into(), text)]), &grant)
        };
        let root = make()?;
        let Value::App(Fux::Record(record)) = &root else {
            return Err(fail());
        };
        let child = record.get("text")?;
        let held = grant.remaining();
        let copies: Vec<_> = (0..10)
            .flat_map(|_| [root.clone(), child.clone()])
            .collect();
        assert_eq!(grant.remaining(), held, "copies hold nothing more");
        drop(copies);
        assert!(make().is_err());
        drop(root);
        assert!(make().is_err(), "the child keeps its own storage");
        drop(child);
        for _ in 0..100 {
            make()?;
        }
        Ok(())
    }

    #[test]
    fn records_refuse_depth_nodes_and_bytes_before_retention() {
        let big = grant(1 << 20);
        let mut deep = Json::Null;
        for _ in 0..inspect::DEPTH {
            deep = Json::Array(vec![deep]);
        }
        assert!(from_json(&deep, &big).is_err());
        let wide = Json::Array(vec![Json::Bool(false); inspect::NODES]);
        assert!(from_json(&wide, &big).is_err());
        let text = std::iter::repeat_n('a', 1000).collect::<String>();
        let texts = Json::Array(vec![Json::String(text); 9]);
        assert!(from_json(&texts, &grant(8192)).is_err());
        assert!(from_json(&texts, &grant(16384)).is_ok());
    }
}
