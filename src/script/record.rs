//! Immutable local projections. Only checked constructors can retain a record.
use super::{Fux, handle};
use crate::code::{Code, Raise};
use crate::command::{AnyRef, Run};
use crate::inspect::{self, Projection, Scope};
use crate::json::Json;
use crate::wire::Budget;
use fuxth::{AppValue, Failure, List, Value};
use std::rc::Rc;

type V = Value<Fux>;

#[derive(Clone, Debug)]
pub struct Record {
    fields: Vec<(String, V)>,
    weight: u64,
    depth: u8,
    bytes: usize,
    snapshot: Option<inspect::Snapshot>,
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
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
    pub(super) fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match &self.snapshot {
            Some(s) => s.write_text(out),
            None => self.write(out),
        }
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

struct Builder {
    budget: Budget,
    nodes: usize,
}
impl Builder {
    fn new() -> Self {
        Self {
            budget: Budget::with_limit(inspect::BYTES),
            nodes: 0,
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
        self.budget
            .text_value(text)
            .map_err(super::words::observation_failure)?;
        Ok(Value::str(
            self.budget
                .string(text)
                .map_err(super::words::observation_failure)?,
        ))
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
        let mut out = self
            .budget
            .vector(items.len())
            .map_err(super::words::observation_failure)?;
        for item in items {
            out.push(value(self, item)?);
        }
        self.budget
            .slots::<List<Fux>>(1)
            .map_err(super::words::observation_failure)?;
        self.budget
            .slots::<usize>(2)
            .map_err(super::words::observation_failure)?;
        let list = List::new(out).ok_or_else(fail)?;
        Ok(Value::List(Rc::new(list)))
    }
    fn record<const N: usize>(&mut self, fields: [(&str, V); N]) -> Result<V, Failure> {
        self.nodes(N.saturating_add(1))?;
        let mut out = self
            .budget
            .vector(N)
            .map_err(super::words::observation_failure)?;
        for (key, value) in fields {
            out.push((
                self.budget
                    .string(key)
                    .map_err(super::words::observation_failure)?,
                value,
            ));
        }
        self.finish(out)
    }
    fn finish(&mut self, fields: Vec<(String, V)>) -> Result<V, Failure> {
        let depth = fields
            .iter()
            .map(|(_, v)| v.depth())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .filter(|d| *d <= inspect::DEPTH)
            .ok_or_else(fail)?;
        let weight = fields
            .iter()
            .try_fold(1u64, |n, (k, v)| {
                n.checked_add(k.len() as u64)?.checked_add(v.weight())
            })
            .filter(|n| *n <= fuxth::TOTAL)
            .ok_or_else(fail)?;
        let bytes = fields.iter().fold(
            std::mem::size_of::<Record>()
                .saturating_add(std::mem::size_of::<usize>().saturating_mul(2))
                .saturating_add(
                    fields
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(String, V)>()),
                ),
            |n, (k, v)| n.saturating_add(k.capacity()).saturating_add(retained(v)),
        );
        self.budget
            .slots::<Record>(1)
            .map_err(super::words::observation_failure)?;
        self.budget
            .slots::<usize>(2)
            .map_err(super::words::observation_failure)?;
        Ok(Value::App(Fux::Record(Rc::new(Record {
            fields,
            depth,
            weight,
            bytes,
            snapshot: None,
        }))))
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
                let mut out = self
                    .budget
                    .vector(fields.len())
                    .map_err(super::words::observation_failure)?;
                for (key, value) in fields {
                    let key = self
                        .budget
                        .string(key)
                        .map_err(super::words::observation_failure)?;
                    let value = self.json(value, depth.saturating_add(1))?;
                    out.push((key, value));
                }
                self.finish(out)
            }
        }
    }
    fn path(&mut self, path: &inspect::Path) -> Result<V, Failure> {
        let (encoding, text) = match path {
            inspect::Path::Utf8(text) => ("utf8", self.text(text)?),
            inspect::Path::Bytes(bytes) => {
                let mut text = String::new();
                for byte in bytes {
                    self.budget
                        .write(&mut text, format_args!("{byte:02x}"))
                        .map_err(super::words::observation_failure)?;
                }
                ("hex", self.text(&text)?)
            }
        };
        let encoding = self.text(encoding)?;
        self.record([("encoding", encoding), ("text", text)])
    }
}

fn retained(value: &V) -> usize {
    match value {
        Value::Str(s) => s
            .heap_bytes()
            .saturating_add(std::mem::size_of::<fuxth::Text>()),
        Value::List(list) => list.items().iter().fold(
            list.heap_bytes()
                .saturating_add(std::mem::size_of::<List<Fux>>())
                .saturating_add(std::mem::size_of::<usize>().saturating_mul(2)),
            |n, v| n.saturating_add(retained(v)),
        ),
        Value::App(a) => a.text_bytes(),
        Value::Int(_) | Value::Bool(_) => 0,
    }
}

pub(super) fn from_json(value: &Json) -> Result<V, Failure> {
    Builder::new().json(value, 0)
}

macro_rules! record {
    ($b:ident; $($key:literal => $value:expr),* $(,)?) => {{
        let fields = [$(( $key, $value )),*];
        $b.record(fields)
    }};
}

pub(super) fn snapshot(s: inspect::Snapshot) -> Result<V, Failure> {
    let retained = s.heap_bytes()?;
    let b = &mut Builder::new();
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
        let mut fields = b.budget.vector(base.len().saturating_add(2)).map_err(super::words::observation_failure)?;
        for (key, value) in base { fields.push((b.budget.string(key).map_err(super::words::observation_failure)?, value)); }
        if let Some(state) = &p.state { fields.push((b.budget.string("state").map_err(super::words::observation_failure)?, b.json(state, 0)?)); }
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
            fields.push((b.budget.string("launch").map_err(super::words::observation_failure)?, recipe));
        }
        b.nodes(fields.len())?;
        b.finish(fields)
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
        Scope::All => Value::App(Fux::All(None)),
        Scope::Target(t) => id(t, run),
        Scope::Client(c) => Value::App(Fux::Client(c, Some(run))),
    };
    let value = record! { b; "run" => b.text(&run.to_string())?, "events" => b.text(&s.events.to_string())?, "scope" => scope,
        "projection" => b.text(match &s.projection { Projection::Facts => "facts", Projection::Topology => "topology", Projection::Recipe => "recipe", Projection::Field(_) => return Err(fail()) })?,
        "workspaces" => workspaces, "tabs" => tabs, "panes" => panes, "clients" => clients
    }?;
    let Value::App(Fux::Record(mut record)) = value else {
        return Err(fail());
    };
    let record_mut = Rc::get_mut(&mut record).ok_or_else(fail)?;
    record_mut.bytes = record_mut.bytes.checked_add(retained).ok_or_else(fail)?;
    // Plain output traverses the retained schema; JSON and get use local values.
    record_mut.weight = record_mut
        .weight
        .checked_mul(2)
        .filter(|n| *n <= fuxth::TOTAL)
        .ok_or_else(fail)?;
    record_mut.snapshot = Some(s);
    Ok(Value::App(Fux::Record(record)))
}

/// Project held native observations directly; never resolve their source again.
pub(super) fn answer_field(
    answer: &super::Answer,
    run: Option<Run>,
    field: &str,
) -> Result<V, Failure> {
    use crate::answer::PaneAnswer;
    let b = &mut Builder::new();
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
                        let text = crate::json::render(&mut b.budget, |out| {
                            crate::capture::write_line(out, line, c.marks, false, &mut Vec::new())
                        })
                        .map_err(super::words::observation_failure)?;
                        b.text(&text)
                    });
                }
                "styles" if c.styles => {
                    return b.list(&c.lines, |b, line| {
                        let mut values = b
                            .budget
                            .vector(
                                line.iter()
                                    .filter(|cut| cut.style != fux_vt::Attributes::default())
                                    .count(),
                            )
                            .map_err(super::words::observation_failure)?;
                        let mut col = 0u32;
                        for cut in line {
                            if cut.style != fux_vt::Attributes::default() {
                                let mut fields = b
                                    .budget
                                    .vector(2)
                                    .map_err(super::words::observation_failure)?;
                                for (name, value) in
                                    [("col", number(col)?), ("len", number(cut.cells)?)]
                                {
                                    fields.push((
                                        b.budget
                                            .string(name)
                                            .map_err(super::words::observation_failure)?,
                                        value,
                                    ));
                                }
                                for (name, value) in crate::capture::style_fields(cut.style) {
                                    let value = match value {
                                        crate::capture::StyleValue::Flag => Value::Bool(true),
                                        crate::capture::StyleValue::Color(fux_vt::Color::Idx(
                                            n,
                                        )) => number(n)?,
                                        crate::capture::StyleValue::Color(fux_vt::Color::Rgb(
                                            fux_vt::Rgb { r, g, b: blue },
                                        )) => b.text(&format!("#{r:02x}{g:02x}{blue:02x}"))?,
                                        crate::capture::StyleValue::Color(
                                            fux_vt::Color::Default,
                                        ) => continue,
                                    };
                                    b.budget
                                        .reserve(&mut fields, 1)
                                        .map_err(super::words::observation_failure)?;
                                    fields.push((
                                        b.budget
                                            .string(name)
                                            .map_err(super::words::observation_failure)?,
                                        value,
                                    ));
                                }
                                b.nodes(fields.len().saturating_add(1))?;
                                values.push(b.finish(fields)?);
                            }
                            col = col.checked_add(cut.cells).ok_or_else(fail)?;
                        }
                        b.nodes(values.len())?;
                        b.budget
                            .slots::<List<Fux>>(1)
                            .map_err(super::words::observation_failure)?;
                        b.budget
                            .slots::<usize>(2)
                            .map_err(super::words::observation_failure)?;
                        Ok(Value::List(Rc::new(List::new(values).ok_or_else(fail)?)))
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
        super::Answer::Status(status) => status
            .get(field)
            .map(|v| b.json(v, 0))
            .unwrap_or_else(|| Err(absent())),
        super::Answer::Panes(_) => Err(absent()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn records_refuse_depth_nodes_and_bytes_before_retention() {
        let mut deep = Json::Null;
        for _ in 0..inspect::DEPTH {
            deep = Json::Array(vec![deep]);
        }
        assert!(from_json(&deep).is_err());
        assert!(from_json(&Json::Array(vec![Json::Bool(false); inspect::NODES])).is_err());
        let text = std::iter::repeat_n('a', fuxth::LONGEST).collect::<String>();
        assert!(from_json(&Json::Array(vec![Json::String(text); 9])).is_err());
        let mut b = Builder::new();
        assert!(
            b.text(&std::iter::repeat_n('a', fuxth::LONGEST.saturating_add(1)).collect::<String>())
                .is_err()
        );
    }
}
