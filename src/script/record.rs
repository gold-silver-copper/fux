//! Immutable local projections. Only checked constructors can retain a record.
use super::{Fux, handle};
use fux_wire::code::{Code, Raise};
use fux_wire::command::{AnyRef, Run};
use fux_wire::inspect::{self, Projection, Scope};
use fux_wire::json::Json;
use fuxth::{Failure, List, Value};
use std::rc::Rc;

type V = Value<Fux>;

#[derive(Clone, Debug)]
pub struct Record {
    pub(super) fields: Vec<(String, V)>,
    pub(super) of: Option<Run>,
    /// A `next-events` batch, which the command line prints as its events.
    batch: bool,
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
    /// What the command line prints of it, if not itself: a batch's
    /// events alone; a batch that missed entries, refused.
    pub(super) fn printed_events(&self) -> Result<Option<V>, Failure> {
        if !self.batch {
            return Ok(None);
        }
        if let Value::Int(missed @ 1..) = self.get("missed")? {
            return Err(missed_entries(missed));
        }
        self.get("events").map(Some)
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

/// Journal entries lost before a cursor, which the reader must resnapshot
/// for, as `events` says too.
fn missed_entries(missed: i64) -> Failure {
    Failure::new(
        fuxth::Code::Fail,
        format!("{missed} journal entries missed; resnapshot before continuing"),
    )
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
        let list = List::new(out);
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
        let batch = false;
        Ok(Value::App(Fux::Record(Rc::new(Record {
            fields,
            of,
            batch,
        }))))
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

/// A failure's fields, as a handler reads them: its `code`, its `number`
/// in Forth's table, its `message`, the `value` it carries (a failed wait's
/// answer) and the `actions` that had taken effect before it, each where
/// its word is.
pub(super) fn failure(thrown: &fuxth::Thrown<Fux>) -> Record {
    let number = thrown.number.map_or_else(null, Value::Int);
    let actions = thrown.actions.iter();
    let actions = actions.map(|t| Value::str(format!("{}: {}", t.place, t.text)));
    let fields = [
        ("code", Value::str(thrown.error.code.name())),
        ("number", number),
        ("message", Value::str(&*thrown.error.message)),
        ("value", thrown.carried.clone().unwrap_or_else(null)),
        (
            "actions",
            Value::List(Rc::new(List::new(actions.collect()))),
        ),
    ];
    let fields = fields.map(|(key, value)| (key.to_owned(), value)).into();
    Record {
        fields,
        of: None,
        batch: false,
    }
}

/// The dictionary's record of a word: its `name`; its `provenance`, where
/// it came from: `core`, `fux`, `library`, or `run` for the program's own
/// (a config's too); its `effect` and `documentation`, as declared, or
/// null for the program's own; its `definition`, the words that made it as
/// they were read, `native` for a word in Rust, or null where a source it
/// was read from couldn't keep them; and its `codes`, the codes it can
/// fail with as it runs, each once (`Machine::codes`), or null where they
/// are unknown.
pub(super) fn word(definition: &fuxth::Definition<'_>, codes: Option<&[fuxth::Code]>) -> V {
    use fuxth::Origin;
    let provenance = match definition.origin {
        Origin::Core => "core",
        Origin::Host => "fux",
        Origin::Library => "library",
        Origin::Program => "run",
    };
    // fux's own words say what they do in its table.
    let fuxs = || Some(super::Word::named(definition.name)?.row().about);
    let about = match definition.origin {
        Origin::Host => fuxs(),
        Origin::Core | Origin::Library | Origin::Program => definition.about,
    };
    let text = |text: Option<&str>| text.map_or_else(null, Value::str);
    let source = match definition.native() {
        true => Value::str("native"),
        false => definition.source().map_or_else(null, Value::str),
    };
    let fields = [
        ("name", Value::str(definition.name)),
        ("provenance", Value::str(provenance)),
        ("effect", text(definition.effect)),
        ("documentation", text(about)),
        ("definition", source),
        (
            "codes",
            codes.map_or_else(null, |codes| {
                let names = codes.iter().map(|code| Value::str(code.name()));
                Value::List(Rc::new(List::new(names.collect())))
            }),
        ),
    ];
    let fields = fields.map(|(key, value)| (key.to_owned(), value)).into();
    let record = Record {
        fields,
        of: None,
        batch: false,
    };
    Value::App(Fux::Record(Rc::new(record)))
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
            Actor::Server | Actor::Outside => b.record([("kind", kind)])?,
        };
        // An entry is the one before the cursor after it.
        let seq = number(item.next.seq.0.saturating_sub(1))?;
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
            // A workspace's event: the workspace it is in, null at the top.
            (
                "parent",
                (item.tab.is_none() && item.workspace.is_some())
                    .then(|| at(item.parent.map(AnyRef::Workspace)).unwrap_or_else(null)),
            ),
        ];
        let Json::Object(data) = &item.data else {
            return Err(fail());
        };
        let count = data
            .len()
            .saturating_add(ids.iter().filter(|(_, v)| v.is_some()).count())
            .saturating_add(3);
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
        out.push(("seq".to_owned(), seq));
        out.push(("next".to_owned(), next));
        b.finish(out, None)
    })?;
    let first = b.text(&batch.first.to_string())?;
    let next = b.text(&batch.next.to_string())?;
    let status = b.text(batch.status.name())?;
    let reason = b.optional(reason)?;
    let fields = [
        ("events", events),
        ("first", first),
        ("next", next),
        ("missed", number(batch.missed)?),
        ("status", status),
        ("reason", reason),
    ];
    let fields = fields.map(|(key, value)| (key.to_owned(), value)).into();
    let record = Record {
        fields,
        of: None,
        batch: true,
    };
    Ok(Value::App(Fux::Record(Rc::new(record))))
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
    let workspaces = b.list(&s.workspaces, |b, w| {
        record! { b;
            "id" => id(AnyRef::Workspace(w.id), run),
            "parent" => w.parent.map(|p| id(AnyRef::Workspace(p), run)).unwrap_or_else(null),
            "name" => b.optional(w.name.as_deref())?,
            "label" => b.text(&w.label)?, "children" => b.list(&w.children, |_,c| Ok(id(*c, run)))?
        }
    })?;
    let tabs = b.list(&s.tabs, |b,t| {
        let nodes = b.list(&t.nodes, |b,n| match n {
            fux_wire::layout::Node::Pane(p) => Ok(id(AnyRef::Pane(*p), run)),
            fux_wire::layout::Node::Split { axis, first, second, first_share, second_share } => {
                let row = [b.text(match axis { fux_wire::layout::Axis::Horizontal => "horizontal", fux_wire::layout::Axis::Vertical => "vertical" })?, number(first_share.get())?, number(second_share.get())?, number(*first)?, number(*second)?];
                b.list(&row, |_,v| Ok(v.clone()))
            }
            fux_wire::layout::Node::Stack { children } => {
                let row = [b.text("stack")?, b.list(children, |_,c| number(*c))?];
                b.list(&row, |_,v| Ok(v.clone()))
            }
        })?;
        record! { b; "id" => id(AnyRef::Tab(t.id),run), "workspace" => id(AnyRef::Workspace(t.workspace),run),
            "name" => b.optional(t.name.as_deref())?, "label" => b.text(&t.label)?,
            "fit" => b.text(if t.fixed { "fixed" } else { "clients" })?, "cols" => number(t.cols)?, "rows" => number(t.rows)?,
            "host" => t.host.map(|c| Value::App(Fux::Client(c,Some(run)))).unwrap_or_else(null),
            "nodes" => nodes, "layout_revision" => t.revision.map(|r| b.text(&r.to_string())).unwrap_or_else(|| Ok(null()))?,
            "zoomed" => t.zoomed.map(|p| id(AnyRef::Pane(p),run)).unwrap_or_else(null)
        }
    })?;
    let panes = b.list(&s.panes, |b,p| {
        let base = [
            ("id", id(AnyRef::Pane(p.id),run)), ("tab", id(AnyRef::Tab(p.tab),run)),
            ("name", b.optional(p.name.as_deref())?), ("label", b.text(&p.label)?), ("title", b.text(&p.title)?),
            ("pid", p.pid.map(number).unwrap_or_else(|| Ok(null()))?),
            ("rows", number(p.rows)?), ("cols", number(p.cols)?),
            ("row", p.row.map(number).unwrap_or_else(|| Ok(null()))?), ("col", p.col.map(number).unwrap_or_else(|| Ok(null()))?),
            ("version", b.text(&p.version.to_string())?),
        ];
        let mut fields = Vec::with_capacity(base.len().saturating_add(3));
        for (key, value) in base { fields.push((key.to_owned(), value)); }
        if let Some(state) = &p.state { fields.push(("state".to_owned(), b.json(state, 0)?)); }
        if let Some(l) = &p.launch {
            let launch = record! { b; "choice" => b.text(if l.shell { "shell" } else { "program" })?,
                "argv" => b.list(&l.argv, |b,s| b.text(s))?, "cwd" => b.path(&l.cwd)?
            }?;
            fields.push(("launch".to_owned(), launch));
        }
        if let Some(cwd) = &p.cwd {
            let cwd = match cwd {
                inspect::Directory::Known(path) => record! { b; "status" => b.text("known")?, "path" => b.path(path)? }?,
                inspect::Directory::Unknown => null(),
                inspect::Directory::Failed(errno) => record! { b; "status" => b.text("failed")?, "operation" => b.text("cwd")?, "errno" => number(*errno)? }?,
            };
            fields.push(("cwd".to_owned(), cwd));
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
        "seats" => b.list(&c.seats, |b,s| record! { b;
            "in" => s.within.map(|w| id(w,run)).unwrap_or_else(null), "current" => id(s.current,run),
            "last" => s.last.map(|l| id(l,run)).unwrap_or_else(null)
        })?,
        "choices" => b.list(&c.choices, |b,choice| record! { b;
            "layout_revision" => b.text(&choice.revision.to_string())?, "stack" => number(choice.stack)?,
            "current" => number(choice.current)?, "last" => choice.last.map(number).unwrap_or_else(|| Ok(null()))?
        })?,
        "opening" => c.opening.map(|o| b.text(o.name())).unwrap_or_else(|| Ok(null()))?
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
        ChangedRows "changed_rows",
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

/// A capture's own field, if it has it: `styles` only if read with them,
/// `changed_rows` only if read `since` a version.
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
        CaptureField::ChangedRows => b.list(c.changed_rows.as_ref()?, |_, row| number(*row)),
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
    Ok(Value::List(Rc::new(List::new(values))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_records_keep_typed_identities() -> Result<(), Failure> {
        use fux_wire::events::{Actor, Batch, Cursor, Item, Status};
        use fux_wire::id::{ClientId, PaneId, TabId, WsId};
        let next: Cursor = "1:1-0".parse().map_err(|()| fail())?;
        for origin in [
            Actor::Outside,
            Actor::Server,
            Actor::Client(ClientId::of(3)),
            Actor::Pane(PaneId::of(4), 5),
            Actor::Program(PaneId::of(4), 5),
        ] {
            let batch = Batch {
                first: next,
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
                    parent: None,
                    client: Some(ClientId::of(4)),
                    moved_from: Some((WsId::of(5), TabId::of(6))),
                    origin,
                    next,
                }],
            };
            let root = events(batch)?;
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
                    "seq",
                    "next"
                ]
            );
            assert_eq!(event.get("seq")?, Value::Int(0));
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
                Actor::Outside | Actor::Server => {}
            }
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
    fn records_refuse_what_nests_too_deep() {
        let mut deep = Json::Null;
        for _ in 0..inspect::DEPTH {
            deep = Json::Array(vec![deep]);
        }
        assert!(from_json(&deep).is_err());
        let Json::Array(within) = deep else {
            return;
        };
        assert!(within.first().is_some_and(|v| from_json(v).is_ok()));
    }
}
