//! Grift evaluates policy; native calls marshal into the existing request vocabulary.
use super::*;
use fux_wire::wire::{Reader, Sink, Wire};
use grift::{Error, Grift, Value as G};

#[derive(Clone, Debug)]
pub struct Machine {
    pub source: String,
    pub name: String,
    pub path: Option<std::path::PathBuf>,
    config: Option<Config>,
    args: Option<Vec<fux_wire::callback::Data>>,
    callback: bool,
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
    let (source, callback) = match program {
        crate::screens::Program::Line(s) => (s.clone(), false),
        crate::screens::Program::Code(c) => (c.source.clone(), true),
    };
    Ok(Machine {
        config: config.cloned(),
        args: args.map(<[_]>::to_vec),
        callback,
        ..Machine::source("key", &source)
    })
}
struct Runtime<'a, 'b> {
    host: &'a mut Host<'b>,
    asks: &'a mut dyn Asks,
    stop: Option<&'a Stop>,
    configuring: bool,
    exiting: bool,
    library: Option<grift::Kept>,
}
type Interpreter<'a, 'b> = Grift<Runtime<'a, 'b>>;
fn failure(g: &mut Interpreter<'_, '_>, f: Failure) -> Error {
    let value = g.string(&f.message).unwrap_or(G::Inert);
    g.error(f.code.name(), &[value])
}
pub(super) fn failed<T>(g: &Grift<T>, e: Error) -> Failed {
    let mut message = String::new();
    let _ = (|| -> std::fmt::Result {
        message.push_str(g.symbol_name(&e.kind).unwrap_or("fail"));
        for value in &e.values {
            message.push(' ');
            write_data(g, value, &mut message)?;
        }
        for (i, form) in e.trace().iter().enumerate() {
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
fn check(g: &mut Interpreter<'_, '_>, state: &mut Runtime<'_, '_>, _: &[G]) -> Result<G, Error> {
    if let Some(stop) = state.stop {
        stop.check()
            .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
    }
    Ok(G::Inert)
}
fn output(
    g: &mut Interpreter<'_, '_>,
    state: &mut Runtime<'_, '_>,
    args: &[G],
) -> Result<G, Error> {
    for argument in args {
        let result = if let Some(text) = g.as_str(argument)? {
            state.host.out.write_all(text.as_bytes())
        } else if native_data(g, argument)? {
            let value = from_grift(g, argument)?;
            write!(state.host.out, "{}", super::value::Text(&value))
        } else {
            write!(state.host.out, "{}", DataText { g, value: argument })
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
struct DataText<'g, 'a, 'b> {
    g: &'g Interpreter<'a, 'b>,
    value: &'g G,
}
impl std::fmt::Display for DataText<'_, '_, '_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_data(self.g, self.value, out)
    }
}

fn data_text(g: &mut Interpreter<'_, '_>, _: &mut Runtime<'_, '_>, args: &[G]) -> Result<G, Error> {
    let [value] = args else {
        return Err(g.error("arity-error", args));
    };
    let mut text = String::new();
    write_data(g, value, &mut text).map_err(|_| g.error("output", args))?;
    g.string(&text)
}
fn read_source(
    g: &mut Interpreter<'_, '_>,
    state: &mut Runtime<'_, '_>,
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
        .map_err(|e| failure(g, Failure::new(Code::Require, e)))?;
    let forms = g.read_in(&name, &source)?;
    state.host.files.required.push(file.clone());
    state.host.files.loading.push(file.clone());
    state.host.files.parents.push(state.host.file.replace(file));
    g.list(&[forms])
}
fn finish_source(
    g: &mut Interpreter<'_, '_>,
    state: &mut Runtime<'_, '_>,
    args: &[G],
) -> Result<G, Error> {
    if !args.is_empty() {
        return Err(g.error("arity-error", args));
    }
    state.host.file = state.host.files.parents.pop().flatten();
    state.host.files.loading.pop();
    Ok(G::Inert)
}

fn request(
    g: &mut Interpreter<'_, '_>,
    state: &mut Runtime<'_, '_>,
    args: &[G],
) -> Result<G, Error> {
    check(g, state, &[])?;
    let Some((G::Symbol(name), args)) = args.split_first() else {
        return Err(g.error("type-error", args));
    };
    let name = g.symbol_name(name)?.to_owned();
    if name == "kind" {
        let [v] = args else {
            return Err(g.error("arity-error", args));
        };
        let kind = match v {
            G::Context(_) => Some("context"),
            G::Operative(_) => Some("operative"),
            G::Applicative(_) => Some("applicative"),
            G::Inert => Some("inert"),
            G::Ignore => Some("ignore"),
            _ => None,
        };
        if let Some(kind) = kind {
            return g.string(kind);
        }
        let v = from_grift(g, v)?;
        let name = match &v {
            Value::App(a) => a.name(),
            _ => v.kind(),
        };
        return g.string(name);
    }
    if name == "string" {
        let [v] = args else {
            return Err(g.error("arity-error", args));
        };
        let v = from_grift(g, v)?;
        let text = v.text().map_err(|f| failure(g, f))?;
        return g.string(&text);
    }
    let Some(word) = Word::named(&name) else {
        return Err(g.error("unbound-symbol", args));
    };
    let takes = word
        .row()
        .effect
        .split_once("--")
        .map_or(0, |(s, _)| s.split_whitespace().count());
    if args.len() != takes {
        return Err(g.error("arity-error", args));
    }
    if matches!(word, Word::Act(Act::Word | Act::Dictionary)) {
        return dictionary(g, state, args);
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
    if matches!(word, Word::Act(Act::Pane | Act::Tab | Act::Workspace))
        && let [Value::Int(number)] = values.as_slice()
    {
        let prefix = match word {
            Word::Act(Act::Pane) => "%",
            Word::Act(Act::Tab) => "@",
            _ => "+",
        };
        let id = format!("{prefix}{number}")
            .parse::<AnyRef>()
            .map_err(|()| g.error("range-error", args))?;
        return to_grift(g, &Value::App(handle(id, None)));
    }

    if matches!(
        word,
        Word::Act(Act::Pane | Act::Tab | Act::Workspace | Act::Client)
    ) {
        if let Some(Value::Int(n)) = values.first() {
            let prefix = match word {
                Word::Act(Act::Pane) => "%",
                Word::Act(Act::Tab) => "@",
                Word::Act(Act::Workspace) => "+",
                _ => "c",
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
    let id = Word::ALL
        .iter()
        .position(|w| *w == word)
        .ok_or_else(|| g.error("unbound-symbol", args))?;
    let mut out = Vec::new();
    let done = match state.host.call(id, values, &mut out) {
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
                state
                    .asks
                    .checkpoint(draft, state.host.context.clone())
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
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
                state.host.actions.push(name.clone());
            }
            out = answered(reply).map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
        }
        Done::Later(Ask::Local(what)) => {
            if matches!(what, Local::Attach { .. }) {
                state
                    .host
                    .prepare()
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
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
                state
                    .asks
                    .checkpoint(draft, state.host.context.clone())
                    .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
            }
            out = state
                .asks
                .local(&what, state.host.state.as_deref_mut(), state.host.out)
                .map_err(|f| failure(g, Failure::new(f.code, f.message)))?;
            if matches!(what, Local::Start | Local::Attach { .. }) {
                state.host.actions.push(name.clone());
            }
            if matches!(what, Local::Shell(Shell::Exit(_))) {
                runtime_exit(g, state);
                return Err(g.error("interrupted", &[]));
            }
        }
        Done::Later(Ask::Dictionary(_) | Ask::Code(_)) => {
            return Err(g.error("unbound-symbol", args));
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
fn dictionary(
    g: &mut Interpreter<'_, '_>,
    state: &Runtime<'_, '_>,
    args: &[G],
) -> Result<G, Error> {
    let top = g.top_level();
    let mut entries = Vec::new();
    let wanted = match args {
        [] => None,
        [v] => g.as_string(v)?,
        _ => return Err(g.error("arity-error", args)),
    };
    let mut names = g.names(&top)?;
    if let Some(library) = &state.library {
        names.extend(g.names(&g.kept(library)?)?);
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
        let Some(v) = g.lookup(&top, &name)? else {
            continue;
        };
        let record = g.context(None)?;
        let original = state
            .library
            .as_ref()
            .map(|kept| g.kept(kept))
            .transpose()?;
        let original = match original {
            Some(context) => g.lookup(&context, &name)?,
            None => None,
        };
        let standard = original.as_ref() == Some(&v);
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
            g.bind(&record, &G::Symbol(key), value)?;
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
        g.bind(&record, &G::Symbol(key), codes)?;
        entries.push(record);
    }
    if wanted.is_some() {
        entries.pop().ok_or_else(|| g.error("unbound-symbol", args))
    } else {
        g.list(&entries)
    }
}
pub(super) fn run(
    machine: &mut Machine,
    host: &mut Host<'_>,
    asks: &mut dyn Asks,
    stop: Option<&Stop>,
) -> Result<Vec<Value<Fux>>, Failed> {
    let mut g = Grift::new().map_err(|_| Failed::new(Code::Memory, "initializing grift"))?;
    for (name, function) in [
        (
            "native-request",
            request as grift::HostFunction<Runtime<'_, '_>>,
        ),
        ("display", output),
        ("data-text", data_text),
        ("read-source", read_source),
        ("finish-source", finish_source),
        ("native-check-stop", check),
    ] {
        let f = g.host_function(name, function).map_err(|e| failed(&g, e))?;
        g.define(name, f).map_err(|e| failed(&g, e))?;
    }
    let mut runtime = Runtime {
        host,
        asks,
        stop,
        configuring: false,
        exiting: false,
        library: None,
    };
    let mut definitions = String::new();
    for word in Word::ALL {
        let name = word.row().name;
        if name == "data-text" {
            continue;
        }
        definitions.push_str(&format!(
            "(define! {name} (lambda! args (apply native-request (pair (quote! {name}) args))))\n"
        ));
    }
    definitions.push_str("(define! kind (lambda! (v) (native-request (quote! kind) v)))\n(define! string (lambda! (v) (native-request (quote! string) v)))\n");
    let top = g.top_level();
    g.evaluate_in(&mut runtime, &top, "native operations", &definitions)
        .map_err(|e| failed(&g, e))?;
    g.evaluate_in(&mut runtime, &top, "library.grift", LIBRARY)
        .map_err(|e| failed(&g, e))?;
    g.evaluate_in(
        &mut runtime,
        &top,
        "sessions.grift",
        include_str!("sessions.grift"),
    )
    .map_err(|e| failed(&g, e))?;
    let baseline = g.context(None).map_err(|e| failed(&g, e))?;
    // Grift publishes its standard names as data; the host owns the only child
    // context used for programs. Save both sets before user code can shadow them.
    let built_in_names = g.symbol("built-in-names").map_err(|e| failed(&g, e))?;
    let built_in_names = g
        .lookup(&top, &built_in_names)
        .map_err(|e| failed(&g, e))?
        .ok_or_else(|| Failed::new(Code::UnknownWord, "built-in-names"))?;
    let mut names = g.list_items(&built_in_names).map_err(|e| failed(&g, e))?;
    names.extend(
        g.names(&top)
            .map_err(|e| failed(&g, e))?
            .into_iter()
            .map(G::Symbol),
    );
    for name in names {
        let G::Symbol(name) = name else {
            return Err(Failed::new(
                Code::Type,
                "built-in-names contains a non-symbol",
            ));
        };
        if let Some(value) = g.lookup(&top, &name).map_err(|e| failed(&g, e))? {
            g.bind(&baseline, &G::Symbol(name), value)
                .map_err(|e| failed(&g, e))?;
        }
    }
    runtime.library = Some(g.keep(baseline).map_err(|e| failed(&g, e))?);
    g.seal_library();
    if let Some(config) = &machine.config {
        let previous = runtime.host.state.as_deref().cloned();
        if let Some(draft) = runtime.host.state.as_deref_mut() {
            *draft = default_state()?.draft();
        }
        let context = std::mem::take(&mut runtime.host.context);
        runtime.configuring = true;
        runtime.host.files = source::Files::frozen(Arc::clone(config));
        runtime.host.file = config.path.clone();
        let loaded = g.evaluate_in(&mut runtime, &top, &config.name, &config.text);
        runtime.host.context = context;
        runtime.configuring = false;
        runtime.host.files = source::Files::sealed();
        runtime.host.file = None;
        if let (Some(draft), Some(previous)) = (runtime.host.state.as_deref_mut(), previous) {
            *draft = previous;
        }
        loaded.map_err(|e| failed(&g, e))?;
    }
    if let Some(stop) = stop {
        stop.check()?;
        if let Ok(mut held) = stop.interrupt.lock() {
            *held = Some(g.interrupter());
        }
    }
    runtime.host.file = machine.path.clone();
    let program = if machine.callback {
        let mut args = Vec::new();
        for arg in machine.args.as_deref().unwrap_or_default() {
            let v = callback::value(arg)?;
            args.push(to_grift(&mut g, &v).map_err(|e| failed(&g, e))?);
        }
        let args = g.list(&args).map_err(|e| failed(&g, e))?;
        g.define("callback-arguments", args)
            .map_err(|e| failed(&g, e))?;
        format!(
            "(native-check-stop) (apply {} callback-arguments)",
            machine.source
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
    let result = g.evaluate_in(&mut runtime, &top, &machine.name, &program);
    if let Some(stop) = stop
        && let Ok(mut held) = stop.interrupt.lock()
    {
        *held = None;
    }
    match result {
        Ok(G::Inert) => Ok(Vec::new()),
        Ok(v) => {
            if !native_data(&g, &v).unwrap_or(false)
                && let Ok(items) = g.list_items(&v)
            {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(output_value(&mut g, &item).map_err(|e| failed(&g, e))?);
                }
                Ok(vec![Value::List(Rc::new(List::new(values)))])
            } else {
                output_value(&mut g, &v)
                    .map(|v| vec![v])
                    .map_err(|e| failed(&g, e))
            }
        }
        Err(e) if runtime.exiting && g.is_kind(&e, "interrupted").unwrap_or(false) => {
            Ok(Vec::new())
        }
        Err(e) => {
            let mut f = failed(&g, e);
            f.actions = runtime.host.actions.clone();
            Err(f)
        }
    }
}

// Typed native values are inert data. Bytes carry checked wire encodings for
// options and observations; records remain contexts with ordinary grift fields.
fn to_grift(g: &mut Interpreter<'_, '_>, v: &Value<Fux>) -> Result<G, Error> {
    Ok(match v {
        Value::Printed(text) => g.string(text)?,
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
        Value::App(Fux::Null) => G::Symbol(g.symbol("none")?),
        Value::App(Fux::Code(code)) => {
            let forms = g.read(&code.source)?;
            let form = g
                .list_items(&forms)?
                .into_iter()
                .next()
                .ok_or_else(|| g.error("parse-error", &[]))?;
            let tag = g.symbol("code")?;
            g.list(&[G::Symbol(tag), form])?
        }
        Value::App(Fux::Record(r)) => {
            let context = g.context(None)?;
            for (k, v) in &r.fields {
                let key = g.symbol(k)?;
                let value = to_grift(g, v)?;
                g.bind(&context, &G::Symbol(key), value)?;
            }
            if let Some(run) = r.of {
                let key = g.symbol("observation-run")?;
                let value = g.string(&run.to_string())?;
                g.bind(&context, &G::Symbol(key), value)?;
            }
            context
        }
        Value::App(app) => {
            let bytes = encode_app(app);
            let data = g.bytes(&bytes)?;
            let tag = g.symbol("native-value")?;
            g.list(&[G::Symbol(tag), data])?
        }
    })
}
fn from_grift(g: &mut Interpreter<'_, '_>, v: &G) -> Result<Value<Fux>, Error> {
    from_grift_at(g, v, fux_wire::callback::DEPTH.saturating_add(1))
}
fn from_grift_at(g: &mut Interpreter<'_, '_>, v: &G, depth: u8) -> Result<Value<Fux>, Error> {
    let Some(depth) = depth.checked_sub(1) else {
        return Err(g.error("too-long", &[v.clone()]));
    };
    Ok(match v {
        G::Float(n) => Value::Float(*n),
        G::Integer(n) => Value::Int(*n),
        G::Boolean(b) => Value::Bool(*b),
        G::String(_) => Value::str(g.as_string(v)?.unwrap_or_default()),
        G::Bytes(_) => Value::Bytes(Rc::new(g.as_bytes(v)?.unwrap_or_default().to_vec())),
        G::EmptyList => Value::List(Rc::new(List::new(Vec::new()))),
        G::Symbol(s) if g.symbol_name(s)? == "none" => Value::App(Fux::Null),
        G::Symbol(s) => Value::Symbol(Rc::new(g.symbol_name(s)?.to_owned())),
        G::Pair(_) => {
            let items = g.list_items(v)?;
            match items.as_slice() {
                [G::Symbol(tag), data] if g.symbol_name(tag)? == "native-value" => {
                    let Some(bytes) = g.as_bytes(data)? else {
                        return Err(g.error("type-error", &[data.clone()]));
                    };
                    let app = decode_app(bytes)
                        .map_err(|e| failure(g, Failure::new(Code::BadValue, e.to_string())))?;
                    Value::App(app)
                }
                [G::Symbol(tag), form] if g.symbol_name(tag)? == "code" => {
                    let mut source = String::new();
                    g.write(form, &mut source)
                        .map_err(|_| g.error("bad-value", &[form.clone()]))?;
                    Value::App(Fux::Code(Rc::new(fux_wire::callback::Code { source })))
                }
                _ => Value::List(Rc::new(List::new(
                    items
                        .iter()
                        .map(|v| from_grift_at(g, v, depth))
                        .collect::<Result<_, _>>()?,
                ))),
            }
        }
        G::Context(_) => {
            let mut fields = Vec::new();
            let mut run = None;
            for key in g.names(v)? {
                let name = g.symbol_name(&key)?.to_owned();
                let Some(value) = g.lookup(v, &key)? else {
                    continue;
                };
                if name == "observation-run" {
                    run = g.as_str(&value)?.and_then(|s| s.parse().ok());
                } else {
                    fields.push((name, from_grift_at(g, &value, depth)?));
                }
            }
            record::from_fields(fields, run).map_err(|f| failure(g, f))?
        }
        _ => return Err(g.error("type-error", &[v.clone()])),
    })
}
fn encode_app(v: &Fux) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        Fux::Null => 0u8.put(&mut out),
        Fux::All => 1u8.put(&mut out),
        Fux::Pane(p, r) => {
            2u8.put(&mut out);
            p.put(&mut out);
            r.put(&mut out)
        }
        Fux::Tab(p, r) => {
            3u8.put(&mut out);
            p.put(&mut out);
            r.put(&mut out)
        }
        Fux::Workspace(p, r) => {
            4u8.put(&mut out);
            p.put(&mut out);
            r.put(&mut out)
        }
        Fux::Client(p, r) => {
            5u8.put(&mut out);
            p.put(&mut out);
            r.put(&mut out)
        }
        Fux::Opts(opts) => {
            6u8.put(&mut out);
            opts.put(&mut out)
        }
        Fux::Extent(e) => {
            7u8.put(&mut out);
            e.put(&mut out)
        }
        Fux::Answer(a, run) => match &**a {
            Answer::Pane(p) => {
                8u8.put(&mut out);
                run.put(&mut out);
                p.put(&mut out)
            }
            Answer::Panes(ps) => {
                9u8.put(&mut out);
                run.put(&mut out);
                ps.put(&mut out)
            }
            Answer::Capture(c) => {
                10u8.put(&mut out);
                run.put(&mut out);
                c.put(&mut out)
            }
        },
        Fux::Record(_) | Fux::Code(_) => {}
    };
    out
}
fn decode_app(bytes: &[u8]) -> Result<Fux, fux_wire::wire::Error> {
    let mut r = Reader::new(bytes);
    let tag = u8::get(&mut r)?;
    Ok(match tag {
        0 => Fux::Null,
        1 => Fux::All,
        2 => Fux::Pane(Wire::get(&mut r)?, Wire::get(&mut r)?),
        3 => Fux::Tab(Wire::get(&mut r)?, Wire::get(&mut r)?),
        4 => Fux::Workspace(Wire::get(&mut r)?, Wire::get(&mut r)?),
        5 => Fux::Client(Wire::get(&mut r)?, Wire::get(&mut r)?),
        6 => Fux::Opts(Rc::new(Wire::get(&mut r)?)),
        7 => Fux::Extent(Wire::get(&mut r)?),
        8..=10 => {
            let run = Wire::get(&mut r)?;
            let a = match tag {
                8 => Answer::Pane(Wire::get(&mut r)?),
                9 => Answer::Panes(Wire::get(&mut r)?),
                _ => Answer::Capture(Wire::get(&mut r)?),
            };
            Fux::Answer(Rc::new(a), run)
        }
        _ => return Err(fux_wire::wire::Error::Bad("native value")),
    })
}

impl Wire for Opts {
    fn put(&self, out: &mut impl Sink) {
        self.on.put(out);
        self.of.put(out);
        self.name.put(out);
        self.size.put(out);
        self.cwd.put(out);
        self.argv.put(out);
        self.shell.put(out);
        self.history_bytes.put(out);
        self.reflow.put(out);
        self.alternate_scroll_lines.put(out);
        self.picture_bytes.put(out);
        self.program_clipboard.put(out);
        self.axis.put(out);
        self.shares.put(out);
        self.place.put(out);
        self.kill.put(out);
        self.watch.put(out);
        self.timeout.put(out);
        self.view.put(out);
        self.history.put(out);
        self.inputs.put(out);
        self.given.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, fux_wire::wire::Error> {
        Ok(Self {
            on: Wire::get(r)?,
            of: Wire::get(r)?,
            name: Wire::get(r)?,
            size: Wire::get(r)?,
            cwd: Wire::get(r)?,
            argv: Wire::get(r)?,
            shell: Wire::get(r)?,
            history_bytes: Wire::get(r)?,
            reflow: Wire::get(r)?,
            alternate_scroll_lines: Wire::get(r)?,
            picture_bytes: Wire::get(r)?,
            program_clipboard: Wire::get(r)?,
            axis: Wire::get(r)?,
            shares: Wire::get(r)?,
            place: Wire::get(r)?,
            kill: Wire::get(r)?,
            watch: Wire::get(r)?,
            timeout: Wire::get(r)?,
            view: Wire::get(r)?,
            history: Wire::get(r)?,
            inputs: Wire::get(r)?,
            given: Wire::get(r)?,
        })
    }
}
impl Wire for On {
    fn put(&self, out: &mut impl Sink) {
        match self {
            Self::Target(v) => {
                0u8.put(out);
                v.put(out)
            }
            Self::Client(v) => {
                1u8.put(out);
                v.put(out)
            }
            Self::Version(v) => {
                2u8.put(out);
                v.put(out)
            }
            Self::Changed(v) => {
                3u8.put(out);
                v.put(out)
            }
            Self::Seen(v) => {
                4u8.put(out);
                v.put(out)
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, fux_wire::wire::Error> {
        Ok(match u8::get(r)? {
            0 => Self::Target(Wire::get(r)?),
            1 => Self::Client(Wire::get(r)?),
            2 => Self::Version(Wire::get(r)?),
            3 => Self::Changed(Wire::get(r)?),
            4 => Self::Seen(Wire::get(r)?),
            _ => return Err(fux_wire::wire::Error::Bad("request subject")),
        })
    }
}
impl Wire for Opt {
    fn put(&self, out: &mut impl Sink) {
        let n = Self::ALL
            .iter()
            .position(|v| v == self)
            .and_then(|n| u8::try_from(n).ok())
            .unwrap_or(u8::MAX);
        n.put(out)
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, fux_wire::wire::Error> {
        let n = u8::get(r)?;
        Self::ALL
            .get(usize::from(n))
            .copied()
            .ok_or(fux_wire::wire::Error::Bad("option"))
    }
}

fn native_data(g: &Interpreter<'_, '_>, v: &G) -> Result<bool, Error> {
    let Some((G::Symbol(tag), _)) = g.as_pair(v)? else {
        return Ok(false);
    };
    Ok(matches!(g.symbol_name(&tag)?, "native-value" | "code"))
}
/// Walk data with an explicit stack, including a record's field values.
fn write_data<T>(g: &Grift<T>, value: &G, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
    enum Part {
        Value(G),
        Text(&'static str),
        Name(String),
        LeaveContext,
    }
    let mut todo = vec![Part::Value(value.clone())];
    let mut contexts: Vec<G> = Vec::new();
    while let Some(part) = todo.pop() {
        match part {
            Part::LeaveContext => {
                contexts.pop();
            }
            Part::Text(text) => out.write_str(text)?,
            Part::Name(name) => out.write_str(&name)?,
            Part::Value(v) => match &v {
                G::Context(_) => {
                    if contexts.contains(&v) {
                        g.write(&v, out).map_err(|_| std::fmt::Error)?;
                        continue;
                    }
                    contexts.push(v.clone());
                    todo.push(Part::LeaveContext);
                    let names = g.names(&v).map_err(|_| std::fmt::Error)?;
                    out.write_str("(")?;
                    todo.push(Part::Text(")"));
                    let mut fields = Vec::new();
                    for name in names {
                        let text = g
                            .symbol_name(&name)
                            .map_err(|_| std::fmt::Error)?
                            .to_owned();
                        if text == "observation-run" {
                            continue;
                        }
                        let value = g
                            .lookup(&v, &name)
                            .map_err(|_| std::fmt::Error)?
                            .ok_or(std::fmt::Error)?;
                        fields.push((text, value));
                    }
                    for (i, (name, value)) in fields.into_iter().enumerate().rev() {
                        todo.push(Part::Text(")"));
                        todo.push(Part::Value(value));
                        todo.push(Part::Text(" "));
                        todo.push(Part::Name(name));
                        todo.push(Part::Text(if i == 0 { "(" } else { " (" }));
                    }
                }
                G::Pair(_) => {
                    let items = match g.list_items(&v) {
                        Ok(items) => items,
                        Err(_) => {
                            g.write(&v, out).map_err(|_| std::fmt::Error)?;
                            continue;
                        }
                    };
                    if let [G::Symbol(tag), data] = items.as_slice()
                        && g.symbol_name(tag).map_err(|_| std::fmt::Error)? == "native-value"
                        && let Some(bytes) = g.as_bytes(data).map_err(|_| std::fmt::Error)?
                    {
                        let app = decode_app(bytes).map_err(|_| std::fmt::Error)?;
                        if matches!(app, Fux::Answer(..)) {
                            record::write_data(&Value::App(app), out)?;
                        } else {
                            let text = Value::App(app).text().map_err(|_| std::fmt::Error)?;
                            out.write_str(&super::quoted(&text))?;
                        }
                    } else {
                        out.write_str("(")?;
                        todo.push(Part::Text(")"));
                        for (i, item) in items.into_iter().enumerate().rev() {
                            todo.push(Part::Value(item));
                            if i > 0 {
                                todo.push(Part::Text(" "));
                            }
                        }
                    }
                }
                _ => g.write(&v, out).map_err(|_| std::fmt::Error)?,
            },
        }
    }
    Ok(())
}

fn output_value(g: &mut Interpreter<'_, '_>, v: &G) -> Result<Value<Fux>, Error> {
    if matches!(
        v,
        G::Integer(_) | G::Boolean(_) | G::String(_) | G::Float(_) | G::Bytes(_) | G::Symbol(_)
    ) || native_data(g, v)?
    {
        return from_grift(g, v);
    }
    let mut text = String::new();
    write_data(g, v, &mut text).map_err(|_| g.error("output", &[]))?;
    Ok(Value::Printed(Rc::new(text)))
}

fn runtime_exit(g: &mut Interpreter<'_, '_>, state: &mut Runtime<'_, '_>) {
    state.exiting = true;
    let mut stop = g.interrupter();
    let _ = stop.stop();
}
