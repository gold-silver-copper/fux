//! Grift evaluates policy; native calls marshal into the existing request vocabulary.
use super::*;
use fux_wire::command::Percent;
use grift::{Error, Grift, Value as G};

#[derive(Clone, Debug)]
pub struct Machine {
    pub source: String,
    pub name: String,
    pub path: Option<std::path::PathBuf>,
    config: Option<Config>,
    args: Option<Vec<fux_wire::callback::Data>>,
    callback: bool,
    /// The line of its file a callback's form was written on.
    line: u32,
}
impl Machine {
    pub fn source(name: &str, source: &str) -> Self {
        Self {
            source: source.to_owned(),
            name: name.to_owned(),
            path: None,
            config: None,
            args: None,
            callback: false,
            line: 0,
        }
    }
}
pub fn compile(source: &str) -> Result<Machine, Failed> {
    Ok(Machine::source("program", source))
}
pub fn callback_machine(
    config: Option<&Config>,
    program: &crate::screens::Program,
    args: Option<&[fux_wire::callback::Data]>,
    stop: Option<&Stop>,
) -> Result<Machine, Failed> {
    if let Some(stop) = stop {
        stop.check()?;
    }
    let (source, callback, name, line) = match program {
        crate::screens::Program::Line(s) => (s.clone(), false, "key", 0),
        crate::screens::Program::Code(c) => match &c.place {
            Some(place) => (c.source.clone(), true, place.file.as_str(), place.line),
            None => (c.source.clone(), true, "key", 0),
        },
    };
    Ok(Machine {
        config: config.cloned(),
        args: args.map(<[_]>::to_vec),
        callback,
        line,
        ..Machine::source(name, &source)
    })
}
/// What a run's host functions reach. `A` is where its requests go.
pub struct Runtime<'a, 'b, A: Asks + ?Sized> {
    pub(crate) host: &'a mut Host<'b>,
    pub(crate) asks: &'a mut A,
    stop: Option<&'a Stop>,
    configuring: bool,
    /// Replaying a config already told: what it warns of was said then.
    replaying: bool,
    exiting: bool,
}
impl<'a, 'b, A: Asks + ?Sized> Runtime<'a, 'b, A> {
    pub fn new(host: &'a mut Host<'b>, asks: &'a mut A, stop: Option<&'a Stop>) -> Self {
        Self {
            host,
            asks,
            stop,
            configuring: false,
            replaying: false,
            exiting: false,
        }
    }
}
pub type Interpreter<'a, 'b, A> = Grift<Runtime<'a, 'b, A>>;
fn failure<T>(g: &mut Grift<T>, f: Failure) -> Error {
    let value = g.string(&f.message).unwrap_or(G::Inert);
    g.error(f.code.name(), &[value])
}
pub(super) fn failed<T>(g: &Grift<T>, e: Error) -> Failed {
    let mut message = String::new();
    let _ = (|| -> std::fmt::Result {
        message.push_str(g.symbol_name(&e.kind).unwrap_or("fail"));
        for value in &e.values {
            message.push(' ');
            g.write(value, &mut message).map_err(|_| std::fmt::Error)?;
        }
        let shown = e.trace().iter().filter(|form| {
            !matches!(g.position(form), Ok(Some((source, _)))
                if BOOKS.iter().any(|(name, _)| *name == source))
        });
        for (i, form) in shown.enumerate() {
            use std::fmt::Write;
            // Grift names the source of text that didn't read as a string, the
            // line being the second of the parse error's values.
            if let (Ok(Some(source)), Some(G::Integer(line))) = (g.as_str(form), e.values.get(1)) {
                write!(&mut message, " at {source}:{line}")?;
                continue;
            }
            message.push_str(if i == 0 { " in " } else { "\n  from " });
            g.write(form, &mut message).map_err(|_| std::fmt::Error)?;
            if let Ok(Some((source, line))) = g.position(form) {
                write!(&mut message, " at {source}:{line}")?;
            }
        }
        Ok(())
    })();
    let kind = g.symbol_name(&e.kind).unwrap_or("fail");
    let mut failed = Failed::new(
        kind.parse().unwrap_or(Code::Fail),
        message
            .strip_prefix(kind)
            .unwrap_or(&message)
            .trim_start()
            .to_owned(),
    );
    failed.kind = Some(kind.to_owned());
    failed
}
fn check<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    state: &mut Runtime<'_, '_, A>,
    _: &[G],
) -> Result<G, Error> {
    if let Some(stop) = state.stop {
        stop.check()
            .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
    }
    Ok(G::Inert)
}
fn output<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    state: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    for argument in args {
        let result = if let Some(text) = g.as_str(argument)? {
            state.host.out.write_all(text.as_bytes())
        } else {
            let mut text = String::new();
            write_display(g, argument, &mut text)?;
            state.host.out.write_all(text.as_bytes())
        };
        result.map_err(|error| failure(g, Failure::new(Code::Output, error.to_string())))?;
    }
    state
        .host
        .out
        .flush()
        .map_err(|error| failure(g, Failure::new(Code::Output, error.to_string())))?;
    Ok(G::Inert)
}
fn data_text<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    _: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    let [value] = args else {
        return Err(g.error("arity-error", args));
    };
    let mut text = String::new();
    g.write(value, &mut text)
        .map_err(|_| g.error("output", args))?;
    g.string(&text)
}
fn read_source<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    state: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    let [path, once] = args else {
        return Err(g.error("arity-error", args));
    };
    let path = g
        .as_string(path)?
        .ok_or_else(|| g.error("type-error", args))?;
    let file = state
        .host
        .files
        .resolve(&path, state.host.file.as_deref())
        .map_err(|e| failure(g, Failure::new(Code::Require, e)))?;
    let name = file.display().to_string();
    if state.host.file.as_ref() == Some(&file) || state.host.files.loading.contains(&file) {
        return Err(failure(
            g,
            Failure::new(Code::Require, format!("{name}: import cycle")),
        ));
    }
    if *once == G::Boolean(true) && state.host.files.required.contains(&file) {
        return Ok(G::EmptyList);
    }
    let source = state
        .host
        .files
        .read(&file)
        .map_err(|e| failure(g, Failure::new(Code::Require, format!("{name}: {e}"))))?;
    let forms = g.read_in(&name, &source)?;
    state.host.files.required.push(file.clone());
    state.host.files.loading.push(file.clone());
    state.host.files.parents.push(state.host.file.replace(file));
    g.list(&[forms])
}
fn finish_source<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    state: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    if !args.is_empty() {
        return Err(g.error("arity-error", args));
    }
    state.host.file = state.host.files.parents.pop().flatten();
    state.host.files.loading.pop();
    Ok(G::Inert)
}

fn kind<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    _: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    let [v] = args else {
        return Err(g.error("arity-error", args));
    };
    let kind = match v {
        G::Context(_) => Some("context"),
        G::Operative(_) | G::Primitive(_) => Some("operative"),
        G::Applicative(_) | G::Host(_) => Some("applicative"),
        G::Inert => Some("inert"),
        G::Ignore => Some("ignore"),
        G::EmptyList
        | G::Boolean(_)
        | G::Integer(_)
        | G::Float(_)
        | G::Symbol(_)
        | G::Pair(_)
        | G::String(_)
        | G::Bytes(_) => None,
    };
    if let Some(kind) = kind {
        return g.string(kind);
    }
    if let Some(held) = held_kind(g, v)? {
        return g.string(&held);
    }
    if matches!(v, G::Pair(_))
        && let Ok(items) = g.list_items(v)
        && fields(g, &items)?.is_some()
    {
        return g.string("record");
    }
    let v = from_grift(g, v)?;
    let name = match &v {
        Value::App(a) => a.name(),
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::Symbol(_)
        | Value::List(_)
        | Value::Bytes(_) => v.kind(),
    };
    g.string(name)
}
fn text_of<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    _: &mut Runtime<'_, '_, A>,
    args: &[G],
) -> Result<G, Error> {
    let [v] = args else {
        return Err(g.error("arity-error", args));
    };
    if held_kind(g, v)?.is_some() {
        let mut text = String::new();
        held_text(g, v, &mut text)?;
        return g.string(&text);
    }
    let v = from_grift(g, v)?;
    let text = v.text().map_err(|f| failure(g, f))?;
    g.string(&text)
}

/// The kind of the server's answer `v` is, its first field: read without
/// making a whole capture a value to ask it.
fn held_kind<T>(g: &Grift<T>, v: &G) -> Result<Option<String>, Error> {
    let Some((first, _)) = g.as_pair(v)? else {
        return Ok(None);
    };
    let Some((key, value)) = field(g, &first)? else {
        return Ok(None);
    };
    let kind = g
        .as_string(&value)?
        .filter(|kind| key == "kind" && record::HELD.iter().any(|(name, _)| name == kind));
    Ok(kind)
}

/// A held answer's text, as `Record::write_held` says it.
fn held_text<T>(g: &Grift<T>, v: &G, out: &mut String) -> Result<(), Error> {
    let items = g.list_items(v)?;
    let mut lines = None;
    for item in &items {
        if let Some((key, value)) = field(g, item)? {
            match key.as_str() {
                "text" => {
                    out.push_str(g.as_str(&value)?.unwrap_or_default());
                    return Ok(());
                }
                "lines" => lines = Some(value),
                _ => {}
            }
        }
    }
    let lines = lines.map(|lines| g.list_items(&lines)).transpose()?;
    for (i, line) in lines.unwrap_or_default().iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(g.as_str(line)?.unwrap_or_default());
    }
    Ok(())
}

/// `get` of a value that is a record: its field, no server asked.
fn field_of<T>(g: &mut Grift<T>, record: &G, field: &G) -> Result<Option<G>, Error> {
    let Some(field_name) = g.as_string(field)? else {
        return Ok(None);
    };
    let name = g.symbol(&field_name)?;
    if matches!(record, G::Context(_)) {
        return match g.names(record)?.contains(&name) {
            true => g.lookup(record, &name),
            false => {
                let about = g.string(&field_name)?;
                Err(g.error("no-field", &[about]))
            }
        };
    }
    if !matches!(record, G::Pair(_)) {
        return Ok(None);
    }
    if let Some((G::Symbol(head), _)) = g.as_pair(record)?
        && [CODE, REQUEST].contains(&g.symbol_name(&head)?)
    {
        return Ok(None);
    }
    for entry in g.list_items(record)? {
        if let Some((G::Symbol(key), rest)) = g.as_pair(&entry)?
            && key == name
            && let Some((value, _)) = g.as_pair(&rest)?
        {
            return Ok(Some(value));
        }
    }
    let about = g.string(&field_name)?;
    Err(g.error("no-field", &[about]))
}

/// A word's call as a failure's effect list says it (D-25): its name and
/// values as data, cut short past what a line of a notice shows.
fn call_text<T>(g: &Grift<T>, name: &str, args: &[G]) -> String {
    const MOST: usize = 200;
    let mut text = format!("({name}");
    for arg in args {
        text.push(' ');
        if g.write(arg, &mut text).is_err() || text.len() > MOST {
            break;
        }
    }
    if text.len() > MOST {
        text = text.chars().take(MOST).chain(['…']).collect();
    }
    text.push(')');
    text
}

/// One word of fux's vocabulary, called from grift.
pub(super) fn request<A: Asks + ?Sized>(
    g: &mut Interpreter<'_, '_, A>,
    state: &mut Runtime<'_, '_, A>,
    word: Word,
    args: &[G],
) -> Result<G, Error> {
    check(g, state, &[])?;
    let name = word.row().name;
    let takes = word
        .row()
        .effect
        .split_once("--")
        .map_or(0, |(s, _)| s.split_whitespace().count());
    if args.len() != takes {
        return Err(g.error("arity-error", args));
    }
    if matches!(word, Word::Act(Act::Word | Act::Dictionary)) {
        return dictionary(g, args);
    }
    if matches!(word, Word::Act(Act::Get))
        && let [record, field] = args
        && let Some(found) = field_of(g, record, field)?
    {
        return Ok(found);
    }
    // Reject bounded scalars while they are borrowed; a retained string can be large.
    if matches!(word, Word::Act(Act::Arrange))
        && let [_, revision, nodes] = args
    {
        if let Some(text) = g.as_str(revision)?
            && text.parse::<fux_wire::command::LayoutRevision>().is_err()
        {
            let about = g.string("not a layout revision")?;
            return Err(g.error("bad-value", &[about, revision.clone()]));
        }
        let mut rest = nodes.clone();
        while let Some((node, tail)) = g.as_pair(&rest)? {
            rest = tail;
            if let Some((axis, _)) = g.as_pair(&node)?
                && let Some(text) = g.as_str(&axis)?
                && !matches!(text, "horizontal" | "vertical" | "stack")
            {
                let about = g.string("unknown layout axis")?;
                return Err(g.error("bad-value", &[about, axis]));
            }
        }
    }
    let mut values = args
        .iter()
        .map(|v| from_grift(g, v))
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(
        word,
        Word::Act(Act::Pane | Act::Tab | Act::Workspace | Act::Client)
    ) {
        // An id as fux prints it, its run in it, is that handle (D-26).
        if let Some(Value::Str(s)) = values.first()
            && s.contains(':')
            && let Some(found) = from_handle_name(s).or_else(|| from_handle_name(&format!("w{s}")))
            && scope(&Value::App(found.clone())).is_some()
        {
            return to_grift(g, &Value::App(found));
        }
        if let Some(Value::Int(n)) = values.first() {
            let prefix = match word {
                Word::Act(Act::Pane) => "%",
                Word::Act(Act::Tab) => "@",
                Word::Act(Act::Workspace) => "+",
                Word::Opt(_) | Word::Screen(_) | Word::Keys(_) | Word::Act(_) => "c",
            };
            values = vec![Value::str(format!("{prefix}{n}"))];
        }
        if let Some(Value::Str(s)) = values.first() {
            // FUX_PANE includes its program generation, which remains in Context.
            if matches!(word, Word::Act(Act::Pane))
                && s.matches(':').count() == 1
                && let Some((id, generation)) = s.split_once(':')
                && generation.parse::<u64>().is_ok()
            {
                values = vec![Value::str(id)];
            }
        }
    }
    let mut out = Vec::new();
    let done = match state.host.call(word, values, &mut out) {
        Ok(done) => done,
        Err(failed) => {
            let mut values = vec![g.string(&failed.message)?];
            values.extend_from_slice(args);
            return Err(g.error(failed.code.name(), &values));
        }
    };
    match done {
        Done::Now => {}
        Done::Later(Ask::Server {
            request,
            client,
            of,
        }) => {
            if state.configuring {
                return Err(failure(
                    g,
                    Failure::new(Code::NotConfig, "a config changes only its client state"),
                ));
            }
            if let Some(draft) = state.host.state.as_deref_mut() {
                let pending = draft.changed();
                state
                    .asks
                    .checkpoint(draft, state.host.context.clone())
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
                if pending {
                    state.host.took(COMMITTED.to_owned());
                }
            }
            let mut context = state.host.context.clone();
            context.client = client.or(context.client);
            context.of = of;
            let asked = Asked {
                after: None,
                context,
                request: *request,
            };
            let reply = state
                .asks
                .ask(&asked)
                .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
            if let End::Failed { code, message } = reply.end() {
                let answer =
                    answered(reply).map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
                let mut values = vec![g.string(&message)?];
                for v in answer {
                    values.push(to_grift(g, &v)?);
                }
                return Err(g.error(code.name(), &values));
            }
            if acts(&asked.request) {
                state.host.took(call_text(g, name, args));
            }
            out = answered(reply).map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
        }
        Done::Later(Ask::Local(what)) => {
            let mut what = what;
            if let Local::Attach {
                read_only,
                size,
                configured,
                ..
            } = &mut what
            {
                let typing = size.is_none() && !*read_only;
                *configured = state
                    .host
                    .prepare(typing)
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?
                    .map(Box::new);
            }
            if state.replaying && matches!(what, Local::Shell(Shell::Warn(_))) {
                return Ok(G::Inert);
            }
            if state.configuring
                && !matches!(
                    what,
                    Local::Shell(Shell::Getenv(_) | Shell::Warn(_) | Shell::Args)
                )
            {
                return Err(failure(
                    g,
                    Failure::new(Code::NotConfig, "a config changes only its client state"),
                ));
            }
            if matches!(what, Local::Open(_))
                && let Some(draft) = state.host.state.as_deref_mut()
            {
                let pending = draft.changed();
                state
                    .asks
                    .checkpoint(draft, state.host.context.clone())
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
                if pending {
                    state.host.took(COMMITTED.to_owned());
                }
            }
            out = state
                .asks
                .local(&what, state.host.state.as_deref_mut(), state.host.out)
                .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
            if matches!(what, Local::Start | Local::Attach { .. }) {
                state.host.took(call_text(g, name, args));
            }
            if matches!(what, Local::Shell(Shell::Exit(_))) {
                runtime_exit(g, state);
                return Err(g.error("interrupted", &[]));
            }
        }
    }
    match out.as_slice() {
        [] => Ok(G::Inert),
        [v] => to_grift(g, v),
        xs => {
            let xs = xs
                .iter()
                .map(|v| to_grift(g, v))
                .collect::<Result<Vec<_>, _>>()?;
            g.list(&xs)
        }
    }
}
fn dictionary<A: Asks + ?Sized>(g: &mut Interpreter<'_, '_, A>, args: &[G]) -> Result<G, Error> {
    let program = place(g, PROGRAM)?;
    let words = place(g, WORDS)?;
    let mut entries = Vec::new();
    let wanted = match args {
        [] => None,
        [v] => g.as_string(v)?,
        _ => return Err(g.error("arity-error", args)),
    };
    let built_in = g.symbol("built-in-names")?;
    let built_in = g.lookup(&words, &built_in)?.unwrap_or(G::EmptyList);
    let mut names = g.names(&program)?;
    names.extend(g.names(&words)?);
    for name in g.list_items(&built_in)? {
        if let G::Symbol(name) = name {
            names.push(name);
        }
    }
    let mut names = names
        .into_iter()
        .map(|name| Ok((g.symbol_name(&name)?.to_owned(), name)))
        .collect::<Result<Vec<_>, Error>>()?;
    names.sort_by(|a, b| a.0.cmp(&b.0));
    names.dedup_by(|a, b| a.0 == b.0);
    for (text, name) in names {
        if wanted.as_ref().is_some_and(|n| n != &text) {
            continue;
        }
        let Some(v) = g.lookup(&program, &name)? else {
            continue;
        };
        let mut record = Vec::new();
        let standard = g.lookup(&words, &name)?.as_ref() == Some(&v);
        let native = if standard { Word::named(&text) } else { None };
        let row = native.map(|word| word.row());
        let doc = if standard {
            super::docs()
                .ok()
                .and_then(|docs| docs.iter().find(|doc| doc.name == text))
        } else {
            None
        };
        let provenance = if !standard {
            "run"
        } else if row.is_some() {
            "fux"
        } else if doc.is_some() {
            "library"
        } else {
            "core"
        };
        let mut source = String::new();
        g.write(&v, &mut source)
            .map_err(|_| g.error("output", &[]))?;
        for (name, value) in [
            ("name", Some(text.clone())),
            ("provenance", Some(provenance.to_owned())),
            (
                "effect",
                row.map(|row| row.effect.to_owned())
                    .or_else(|| doc.map(|doc| doc.parameters.clone())),
            ),
            (
                "documentation",
                if standard {
                    crate::help::explain(&text)
                } else {
                    None
                },
            ),
            ("definition", Some(source)),
        ] {
            let key = g.symbol(name)?;
            let value = match value {
                Some(value) => g.string(&value)?,
                None => G::Symbol(g.symbol("none")?),
            };
            record.push(g.list(&[G::Symbol(key), value])?);
        }
        let codes = match native {
            Some(word) => {
                let codes = word
                    .codes()
                    .iter()
                    .map(|code| g.string(code.name()))
                    .collect::<Result<Vec<_>, _>>()?;
                g.list(&codes)?
            }
            None => G::Symbol(g.symbol("none")?),
        };
        let key = g.symbol("codes")?;
        record.push(g.list(&[G::Symbol(key), codes])?);
        entries.push(g.list(&record)?);
    }
    if wanted.is_some() {
        entries.pop().ok_or_else(|| g.error("unbound-symbol", args))
    } else {
        g.list(&entries)
    }
}

/// Where a program's names live, each a context of its own: `WORDS`, fux's
/// words and prelude, a child of grift's built-in names; `PROGRAM`, a child
/// of it, where programs and configs define things; each `BOOKS` library,
/// another child, so nothing a program defines changes what a library
/// calls. The host binds them at grift's own top level, which no program
/// reaches.
const WORDS: &str = "words";
const PROGRAM: &str = "program";

/// The libraries, loaded the first time a word of theirs is called: each
/// its source's name, its text and the words it gives.
const BOOKS: [(&str, &str); 2] = [
    ("library.grift", super::LIBRARY),
    ("sessions.grift", include_str!("sessions.grift")),
];

fn place<T>(g: &mut Grift<T>, name: &str) -> Result<G, Error> {
    let top = g.top_level();
    let symbol = g.symbol(name)?;
    g.lookup(&top, &symbol)?
        .ok_or_else(|| g.error("unbound-symbol", &[G::Symbol(symbol)]))
}

/// The context of library `book`, which is evaluated here the first time.
fn book<'a, 'b, A: Asks + ?Sized>(
    g: &mut Interpreter<'a, 'b, A>,
    state: &mut Runtime<'a, 'b, A>,
    book: usize,
) -> Result<G, Error> {
    let Some(&(name, text)) = BOOKS.get(book) else {
        return Err(g.error("unbound-symbol", &[]));
    };
    let top = g.top_level();
    let symbol = g.symbol(name)?;
    if let Some(found) = g.lookup(&top, &symbol)? {
        return Ok(found);
    }
    let words = place(g, WORDS)?;
    let context = g.context(Some(&words))?;
    g.bind(&top, &G::Symbol(symbol), context.clone())?;
    g.evaluate_in(state, &context, name, text)?;
    Ok(context)
}

/// Calls `name` of a library, not the word in `WORDS` that leads here.
fn library<'a, 'b, A: Asks + ?Sized>(
    g: &mut Interpreter<'a, 'b, A>,
    state: &mut Runtime<'a, 'b, A>,
    index: usize,
    name: &str,
    args: &[G],
) -> Result<G, Error> {
    let context = book(g, state, index)?;
    let symbol = g.symbol(name)?;
    let found = match g.names(&context)?.contains(&symbol) {
        true => g.lookup(&context, &symbol)?,
        false => None,
    };
    let Some(f) = found else {
        return Err(g.error("unbound-symbol", &[G::Symbol(symbol)]));
    };
    g.call(state, &f, args)
}

macro_rules! host_functions {
    ($($name:literal $function:path;)*) => {
        fn host_functions<'a, 'b, A: Asks + ?Sized>() -> Vec<(&'static str, grift::HostFunction<Runtime<'a, 'b, A>>)> {
            vec![$(($name, $function),)*]
        }
    };
}
host_functions! {
    "display" output;
    "data-text" data_text;
    "read-source" read_source;
    "finish-source" finish_source;
    "native-check-stop" check;
    "kind" kind;
    "string" text_of;
}

macro_rules! library_words {
    ($($index:literal: $($name:literal)*;)*) => {
        /// Every word the libraries give.
        #[cfg(test)]
        pub(super) const LIBRARY_WORDS: &[&str] = &[$($($name,)*)*];
        fn library_words<'a, 'b, A: Asks + ?Sized>() -> Vec<(&'static str, grift::HostFunction<Runtime<'a, 'b, A>>)> {
            vec![$($(($name, |g, state, args| library(g, state, $index, $name, args)),)*)*]
        }
    };
}
library_words! {
    0: "assert-kind" "information" "all-history" "observed" "shows" "quiet" "until-ended" "grows"
       "list-session" "me-focused" "outer" "inner" "rename-prompt" "confirm-close" "chooser-row"
       "chooser-find" "chooser-container-row" "chooser-index" "chooser-selected" "chooser-siblings"
       "chooser-count" "chooser-path" "chooser-tree" "choose-among" "choose-move" "choose-pane"
       "edit-history" "batch-events";
    1: "native-data?" "session-data" "dump-text" "dump-recipe" "dump" "dump-first" "dump-directory"
       "dump-warnings" "session-find" "restore-create-options" "restore-session" "listing"
       "events";
}

/// A new interpreter: fux's words and prelude, its libraries not loaded.
pub fn interpreter<'a, 'b, A: Asks + ?Sized>(
    state: &mut Runtime<'a, 'b, A>,
) -> Result<Interpreter<'a, 'b, A>, Failed> {
    let mut g = Grift::new().map_err(|_| Failed::new(Code::Memory, "initializing grift"))?;
    let built = (|| {
        let words = g.private_context()?;
        // The host's own last: `kind`, `string` and `data-text` take any
        // grift value, which a word's arguments can't be.
        for (name, function) in Word::host_functions::<A>()
            .into_iter()
            .chain(library_words::<A>())
            .chain(host_functions::<A>())
        {
            let function = g.host_function(name, function)?;
            let name = g.symbol(name)?;
            g.bind(&words, &G::Symbol(name), function)?;
        }
        g.evaluate_in(
            state,
            &words,
            "prelude.grift",
            include_str!("prelude.grift"),
        )?;
        let program = g.context(Some(&words))?;
        g.define(WORDS, words)?;
        g.define(PROGRAM, program)
    })();
    built.map_err(|e| failed(&g, e))?;
    g.seal_library();
    Ok(g)
}

/// Every library loaded, as a client's worker does ahead of its keys.
pub fn load_libraries<'a, 'b, A: Asks + ?Sized>(
    g: &mut Interpreter<'a, 'b, A>,
    state: &mut Runtime<'a, 'b, A>,
) -> Result<(), Failed> {
    for index in 0..BOOKS.len() {
        book(g, state, index).map_err(|e| failed(g, e))?;
    }
    Ok(())
}

/// Evaluates `config` in `g`, on a draft of `defaults` no one keeps: its
/// definitions stay in `g`, and the client already has its state.
pub fn replay_config<'a, 'b, A: Asks + ?Sized>(
    g: &mut Interpreter<'a, 'b, A>,
    state: &mut Runtime<'a, 'b, A>,
    config: &Config,
    defaults: &ClientState,
) -> Result<(), Failed> {
    let program = place(g, PROGRAM).map_err(|e| failed(g, e))?;
    let previous = state.host.state.as_deref().cloned();
    if let Some(draft) = state.host.state.as_deref_mut() {
        *draft = defaults.draft();
    }
    let context = std::mem::take(&mut state.host.context);
    let files = std::mem::replace(
        &mut state.host.files,
        source::Files::frozen(Arc::clone(config)),
    );
    let file = std::mem::replace(&mut state.host.file, config.path.clone());
    state.configuring = true;
    state.replaying = true;
    // A stop is for a run: what it would stop here is the next job's build,
    // which that job's own check then answers.
    let stop = state.stop.take();
    let loaded = g.evaluate_in(state, &program, &config.name, &config.text);
    state.stop = stop;
    state.host.context = context;
    state.configuring = false;
    state.replaying = false;
    state.host.files = files;
    state.host.file = file;
    if let (Some(draft), Some(previous)) = (state.host.state.as_deref_mut(), previous) {
        *draft = previous;
    }
    loaded.map(drop).map_err(|e| failed(g, e))
}

/// Runs `machine` in `g`, which nothing has run in but a config.
pub fn evaluate_job<'a, 'b, A: Asks + ?Sized>(
    g: &mut Interpreter<'a, 'b, A>,
    state: &mut Runtime<'a, 'b, A>,
    machine: &Machine,
) -> Result<Vec<Value<Fux>>, Failed> {
    if let Some(stop) = state.stop {
        stop.check()?;
        if let Ok(mut held) = stop.interrupt.lock() {
            *held = Some(g.interrupter());
        }
    }
    state.host.file = machine.path.clone();
    let program = place(g, PROGRAM).map_err(|e| failed(g, e))?;
    let source = if machine.callback {
        let mut args = Vec::new();
        for arg in machine.args.as_deref().unwrap_or_default() {
            let v = callback::value(arg)?;
            args.push(to_grift(g, &v).map_err(|e| failed(g, e))?);
        }
        let args = g.list(&args).map_err(|e| failed(g, e))?;
        let name = g.symbol("callback-arguments").map_err(|e| failed(g, e))?;
        g.bind(&program, &G::Symbol(name), args)
            .map_err(|e| failed(g, e))?;
        on_its_line(
            &format!(
                "(native-check-stop) (apply {} callback-arguments)",
                machine.source
            ),
            machine.line,
        )
    } else {
        format!(
            "(native-check-stop) {}",
            if machine.source.starts_with("#!") {
                machine.source.replacen("#!", ";", 1)
            } else {
                machine.source.clone()
            }
        )
    };
    let result = g.evaluate_in(state, &program, &machine.name, &source);
    if let Some(stop) = state.stop
        && let Ok(mut held) = stop.interrupt.lock()
    {
        *held = None;
    }
    match result {
        Ok(G::Inert) => Ok(Vec::new()),
        // `fux status` names a word and calls nothing, which would look done.
        Ok(G::Applicative(_) | G::Operative(_) | G::Primitive(_) | G::Host(_))
            if state.host.cli =>
        {
            let name = machine.source.trim();
            let why = match name.contains(char::is_whitespace) {
                true => "the program's value is a combiner: call it".to_owned(),
                false => format!("{name} is a word: call it, as ({name})"),
            };
            Err(Failed::new(Code::Usage, why))
        }
        Ok(v) => match output_value(g, &v) {
            // A list's items, each said alone; a record's fields are one value.
            Ok(Value::Str(_))
                if matches!(v, G::Pair(_) | G::EmptyList)
                    && g.list_items(&v)
                        .is_ok_and(|items| matches!(fields(g, &items), Ok(None))) =>
            {
                let items = g.list_items(&v).map_err(|e| failed(g, e))?;
                let values = items
                    .iter()
                    .map(|item| output_value(g, item))
                    .collect::<Result<_, _>>()
                    .map_err(|e| failed(g, e))?;
                Ok(vec![Value::List(Rc::new(List::new(values)))])
            }
            Ok(value) => Ok(vec![value]),
            Err(e) => Err(failed(g, e)),
        },
        Err(e) if state.exiting && g.is_kind(&e, "interrupted").unwrap_or(false) => Ok(Vec::new()),
        Err(e) => {
            let mut f = failed(g, e);
            f.actions = state.host.actions.clone();
            Err(f)
        }
    }
}

/// Runs `machine` in a new interpreter.
pub(super) fn run(
    machine: &Machine,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    let mut state = Runtime::new(host, asks, stop);
    let mut g = interpreter(&mut state)?;
    if let Some(config) = &machine.config {
        replay_config(&mut g, &mut state, config, &default_state()?)?;
    }
    evaluate_job(&mut g, &mut state, machine)
}

/// The symbol a handle is as grift data, its server run in it: `%3:RUN`,
/// `@3:RUN`, `w+3:RUN`, `c3:RUN` (`c3:` and `w+3:` with no run, as `+3` reads
/// as a number and `c3` is any symbol); `%all`; a share, `percent:30`.
pub(super) fn handle_name(app: &Fux, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
    let run = |out: &mut dyn std::fmt::Write, run: &Option<Run>| match run {
        Some(run) => write!(out, ":{run}"),
        None => Ok(()),
    };
    match app {
        Fux::Pane(p, r) => {
            write!(out, "{p}")?;
            run(out, r)
        }
        Fux::Tab(t, r) => {
            write!(out, "{t}")?;
            run(out, r)
        }
        Fux::Workspace(w, r) => {
            write!(out, "w{w}")?;
            match r {
                Some(_) => run(out, r),
                None => out.write_char(':'),
            }
        }
        Fux::Client(c, r) => {
            write!(out, "{c}")?;
            match r {
                Some(_) => run(out, r),
                None => out.write_char(':'),
            }
        }
        Fux::All => out.write_str("%all"),
        Fux::Extent(Extent::Percent(n)) => write!(out, "percent:{}", n.get()),
        Fux::Extent(Extent::Cells(n)) => write!(out, "{n}"),
        Fux::Null => out.write_str("none"),
        Fux::Record(_) | Fux::Opts(_) | Fux::Code(_) => Ok(()),
    }
}

/// The value the symbol `name` is, if `handle_name` writes it: `%3:RUN`
/// with its run, and a bare `%3` as a person may type it (D-26).
pub(crate) fn from_handle_name(name: &str) -> Option<Fux> {
    if name == "%all" {
        return Some(Fux::All);
    }
    if let Some(share) = name.strip_prefix("percent:") {
        return Some(Fux::Extent(Extent::Percent(Percent::new(
            share.parse().ok()?,
        )?)));
    }
    let (id, run) = match name.split_once(':') {
        Some((id, "")) => (id, None),
        Some((id, run)) => (id, Some(run.parse().ok()?)),
        None => (name, None),
    };
    if id.starts_with('c') {
        // Any symbol may be `c3`: a client's has its `:`.
        name.contains(':').then_some(())?;
        return Some(Fux::Client(id.parse().ok()?, run));
    }
    let id = id.strip_prefix('w').unwrap_or(id);
    id.starts_with(['%', '@', '+'])
        .then(|| id.parse().ok().map(|id| handle(id, run)))
        .flatten()
}

/// fux's values as grift data, which reads back as it: a handle a symbol, a
/// record a list of `(field value)`, a request `(request! SUBJECT (OPTION
/// ARGUMENT…)…)`, a code value `(code! FORM)`. A value of a server's keeps
/// its run; nothing here is encoded, as a person reads it back.
fn to_grift<T>(g: &mut Grift<T>, v: &Value<Fux>) -> Result<G, Error> {
    Ok(match v {
        Value::Float(n) => G::Float(*n),
        Value::Int(n) => G::Integer(*n),
        Value::Bool(b) => G::Boolean(*b),
        Value::Str(s) => g.string(s)?,
        Value::Symbol(s) => G::Symbol(g.symbol(s)?),
        Value::Bytes(b) => g.bytes(b)?,
        Value::List(xs) => {
            let items = xs
                .items()
                .iter()
                .map(|v| to_grift(g, v))
                .collect::<Result<Vec<_>, _>>()?;
            g.list(&items)?
        }
        Value::App(Fux::Code(code)) => {
            let forms = match &code.place {
                Some(place) => g.read_in(&place.file, &on_its_line(&code.source, place.line))?,
                None => g.read(&code.source)?,
            };
            let form = g
                .list_items(&forms)?
                .into_iter()
                .next()
                .ok_or_else(|| g.error("parse-error", &[]))?;
            let tag = g.symbol(CODE)?;
            g.list(&[G::Symbol(tag), form])?
        }
        Value::App(Fux::Record(r)) => {
            let mut items = Vec::with_capacity(r.fields.len().saturating_add(1));
            for (k, v) in &r.fields {
                let key = g.symbol(k)?;
                let value = to_grift(g, v)?;
                items.push(g.list(&[G::Symbol(key), value])?);
            }
            if let Some(run) = r.of.filter(|_| !r.fields.iter().any(|(k, _)| k == RUN)) {
                let key = g.symbol(RUN)?;
                let value = g.string(&run.to_string())?;
                items.push(g.list(&[G::Symbol(key), value])?);
            }
            g.list(&items)?
        }
        Value::App(Fux::Opts(opts)) => {
            let tag = g.symbol(REQUEST)?;
            let subject = match &opts.subject {
                Some(subject) => to_grift(g, subject)?,
                None => G::Symbol(g.symbol("none")?),
            };
            let mut items = vec![G::Symbol(tag), subject];
            for (option, arguments) in &opts.steps {
                let mut step = vec![G::Symbol(g.symbol(Word::Opt(*option).row().name)?)];
                for argument in arguments {
                    step.push(to_grift(g, argument)?);
                }
                items.push(g.list(&step)?);
            }
            g.list(&items)?
        }
        Value::App(Fux::Extent(Extent::Cells(n))) => G::Integer(i64::from(*n)),
        Value::App(app) => {
            let mut name = String::new();
            handle_name(app, &mut name).map_err(|_| g.error("output", &[]))?;
            G::Symbol(g.symbol(&name)?)
        }
    })
}

/// `form` as text, each combination on the line it was written on after
/// `line`, so a failure deep in a binding names the config's own line.
fn write_form<T>(
    g: &mut Grift<T>,
    form: &G,
    out: &mut String,
    line: &mut u32,
    depth: u8,
) -> Result<(), Error> {
    let items = match (form, depth.checked_sub(1)) {
        (G::Pair(_), Some(_)) => g.list_items(form).ok(),
        _ => None,
    };
    let Some(items) = items else {
        return g
            .write(form, out)
            .map_err(|_| g.error("bad-value", std::slice::from_ref(form)));
    };
    if let Some((_, at)) = g.position(form)?
        && at > *line
    {
        out.extend(std::iter::repeat_n(
            '\n',
            usize::try_from(at.saturating_sub(*line)).unwrap_or(0),
        ));
        *line = at;
    }
    out.push('(');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        write_form(g, item, out, line, depth.saturating_sub(1))?;
    }
    out.push(')');
    Ok(())
}

/// `source` on `line` of its file, as a failure in it then names that line.
fn on_its_line(source: &str, line: u32) -> String {
    let lines = usize::try_from(line.saturating_sub(1)).unwrap_or(0);
    std::iter::repeat_n('\n', lines)
        .chain(source.chars())
        .collect()
}

/// What a run's local changes are called, published before it went on.
const COMMITTED: &str = "(this client's settings and keys)";

/// The head of a code value, a request and the field a run goes in.
const CODE: &str = "code!";
const REQUEST: &str = "request!";
const RUN: &str = "run";

/// The field `item` is: a name, not a handle's, and one value.
fn field<T>(g: &Grift<T>, item: &G) -> Result<Option<(String, G)>, Error> {
    let Some((G::Symbol(key), rest)) = g.as_pair(item)? else {
        return Ok(None);
    };
    let Some((value, G::EmptyList)) = g.as_pair(&rest)? else {
        return Ok(None);
    };
    let key = g.symbol_name(&key)?;
    Ok(
        (from_handle_name(key).is_none() && ![CODE, REQUEST].contains(&key))
            .then(|| (key.to_owned(), value)),
    )
}

/// `items`' fields, if they are a record's: each a field, no name twice.
fn fields<T>(g: &Grift<T>, items: &[G]) -> Result<Option<Vec<(String, G)>>, Error> {
    let mut fields = Vec::with_capacity(items.len());
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Some((key, value)) = field(g, item)? else {
            return Ok(None);
        };
        if !seen.insert(key.clone()) {
            return Ok(None);
        }
        fields.push((key, value));
    }
    Ok((!fields.is_empty()).then_some(fields))
}

/// The server run `fields` say they are of.
fn run_of<T>(g: &Grift<T>, fields: &[(String, G)]) -> Result<Option<Run>, Error> {
    let run = fields.iter().find(|(name, _)| name == RUN);
    match run {
        Some((_, value)) => Ok(g.as_str(value)?.and_then(|text| text.parse().ok())),
        None => Ok(None),
    }
}

fn from_grift<T>(g: &mut Grift<T>, v: &G) -> Result<Value<Fux>, Error> {
    from_grift_at(g, v, fux_wire::callback::DEPTH.saturating_add(1))
}
fn from_grift_at<T>(g: &mut Grift<T>, v: &G, depth: u8) -> Result<Value<Fux>, Error> {
    let Some(depth) = depth.checked_sub(1) else {
        return Err(g.error("too-long", std::slice::from_ref(v)));
    };
    Ok(match v {
        G::Float(n) => Value::Float(*n),
        G::Integer(n) => Value::Int(*n),
        G::Boolean(b) => Value::Bool(*b),
        G::String(_) => Value::str(g.as_string(v)?.unwrap_or_default()),
        G::Bytes(_) => Value::Bytes(Rc::new(g.as_bytes(v)?.unwrap_or_default().to_vec())),
        G::EmptyList => Value::List(Rc::new(List::new(Vec::new()))),
        G::Symbol(s) => {
            let name = g.symbol_name(s)?;
            match (name, from_handle_name(name)) {
                ("none", _) => Value::App(Fux::Null),
                (_, Some(handle)) => Value::App(handle),
                (name, None) => Value::Symbol(Rc::new(name.to_owned())),
            }
        }
        G::Pair(_) => {
            let items = g.list_items(v)?;
            let head = match items.first() {
                Some(G::Symbol(head)) => g.symbol_name(head)?.to_owned(),
                _ => String::new(),
            };
            match (head.as_str(), items.as_slice()) {
                (CODE, [_, form]) => {
                    let mut source = String::new();
                    let first = g.position(form)?.map_or(0, |(_, line)| line);
                    write_form(g, form, &mut source, &mut { first }, depth)?;
                    let place = g.position(form)?.map(|(file, line)| {
                        let file = file.to_owned();
                        Box::new(fux_wire::callback::Place { file, line })
                    });
                    Value::App(Fux::Code(Rc::new(fux_wire::callback::Code {
                        source,
                        place,
                    })))
                }
                (REQUEST, [_, subject, steps @ ..]) => {
                    let subject = match from_grift_at(g, subject, depth)? {
                        Value::App(Fux::Null) => None,
                        subject @ Value::Int(_)
                        | subject @ Value::Float(_)
                        | subject @ Value::Bool(_)
                        | subject @ Value::Str(_)
                        | subject @ Value::Symbol(_)
                        | subject @ Value::List(_)
                        | subject @ Value::App(_)
                        | subject @ Value::Bytes(_) => Some(subject),
                    };
                    let mut replayed = Vec::with_capacity(steps.len());
                    for step in steps {
                        let items = g.list_items(step)?;
                        let Some((G::Symbol(name), arguments)) = items.split_first() else {
                            return Err(g.error("bad-value", std::slice::from_ref(step)));
                        };
                        let Some(Word::Opt(option)) = Word::named(g.symbol_name(name)?) else {
                            return Err(g.error("bad-value", std::slice::from_ref(step)));
                        };
                        let arguments = arguments
                            .iter()
                            .map(|v| from_grift_at(g, v, depth))
                            .collect::<Result<_, _>>()?;
                        replayed.push((option, arguments));
                    }
                    words::replayed(subject, replayed).map_err(|f| failure(g, f))?
                }
                _ => match fields(g, &items)? {
                    Some(fields) if held_kind(g, v)?.is_some() || run_of(g, &fields)?.is_some() => {
                        let run = run_of(g, &fields)?;
                        let mut out = Vec::with_capacity(fields.len());
                        for (name, value) in fields {
                            out.push((name, from_grift_at(g, &value, depth)?));
                        }
                        record::from_fields(out, run).map_err(|f| failure(g, f))?
                    }
                    // A list stays a list, whatever it looks like: only a
                    // server's answer or what holds a run is a record.
                    Some(_) | None => Value::List(Rc::new(List::new(
                        items
                            .iter()
                            .map(|v| from_grift_at(g, v, depth))
                            .collect::<Result<_, _>>()?,
                    ))),
                },
            }
        }
        // What grift made one of: an error's fields.
        G::Context(_) => {
            let mut fields = Vec::new();
            for key in g.names(v)? {
                let name = g.symbol_name(&key)?.to_owned();
                if let Some(value) = g.lookup(v, &key)? {
                    fields.push((name, from_grift_at(g, &value, depth)?));
                }
            }
            record::from_fields(fields, None).map_err(|f| failure(g, f))?
        }
        G::Inert
        | G::Ignore
        | G::Operative(_)
        | G::Applicative(_)
        | G::Primitive(_)
        | G::Host(_) => return Err(g.error("type-error", std::slice::from_ref(v))),
    })
}

/// `v` as `display` says it: a string or a held answer as its text, a handle
/// as its id, anything else as data.
fn write_display<T>(g: &mut Grift<T>, v: &G, out: &mut String) -> Result<(), Error> {
    let shown = match v {
        G::Symbol(_) | G::Pair(_) => match from_grift(g, v) {
            Ok(Value::App(Fux::Record(record))) if record.is_held() => {
                Some(Value::App(Fux::Record(record)))
            }
            Ok(Value::App(
                app @ (Fux::Pane(..)
                | Fux::Tab(..)
                | Fux::Workspace(..)
                | Fux::Client(..)
                | Fux::All),
            )) => Some(Value::App(app)),
            Ok(_) | Err(_) => None,
        },
        G::EmptyList
        | G::Boolean(_)
        | G::Inert
        | G::Ignore
        | G::Integer(_)
        | G::Float(_)
        | G::String(_)
        | G::Bytes(_)
        | G::Context(_)
        | G::Operative(_)
        | G::Applicative(_)
        | G::Primitive(_)
        | G::Host(_) => None,
    };
    match shown {
        Some(value) => value.write_text(out).map_err(|f| failure(g, f)),
        None => g.write(v, out).map_err(|_| g.error("output", &[])),
    }
}

fn output_value<T>(g: &mut Grift<T>, v: &G) -> Result<Value<Fux>, Error> {
    if matches!(
        v,
        G::Integer(_) | G::Boolean(_) | G::String(_) | G::Float(_) | G::Bytes(_) | G::Symbol(_)
    ) {
        return from_grift(g, v);
    }
    if matches!(v, G::Pair(_))
        && let Ok(value @ Value::App(_)) = from_grift(g, v)
    {
        return Ok(value);
    }
    let mut text = String::new();
    g.write(v, &mut text).map_err(|_| g.error("output", &[]))?;
    Ok(Value::str(text))
}

fn runtime_exit<A: Asks + ?Sized>(g: &mut Interpreter<'_, '_, A>, state: &mut Runtime<'_, '_, A>) {
    state.exiting = true;
    let mut stop = g.interrupter();
    let _ = stop.stop();
}
