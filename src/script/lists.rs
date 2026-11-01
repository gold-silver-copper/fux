//! `docs/words.txt` and `docs/records.txt`, written from fux's word table
//! and the libraries' definitions, and from records the record builders
//! make; a test fails while a list is stale (`fux_wire::lists`).
use super::{Fux, Word, record};
use fux_wire::lists::{check, file};
use fuxth::Value;

const TEST: &str = "cargo test -p fux --lib lists";

/// docs/words.txt lists every word a fux program may use, each with its
/// stack effect as help shows it.
#[test]
fn words_list_is_current() -> Result<(), String> {
    let fuxs = Word::ALL.iter().map(|w| (w.row().name, w.row().effect));
    let mut lines = vec!["## fux's words".to_owned()];
    lines.extend(fuxs.map(|(name, effect)| format!("{name} ( {effect} )")));
    lines.push("## the libraries' words, written in fuxth".to_owned());
    let docs = super::docs();
    lines.extend(docs.iter().map(|d| format!("{} ( {} )", d.name, d.effect)));
    lines.push("## the core's words".to_owned());
    let core = fuxth::Builtin::ALL.map(|b| format!("{} ( {} )", b.name(), b.effect()));
    lines.extend(core);
    lines.push("## the language's syntax".to_owned());
    let syntax = fuxth::Parsing::ALL.map(|p| format!("{} {}", p.name(), p.syntax().0));
    lines.extend(syntax);
    let head = format!(
        "Every word a fux program may use, with its stack effect as `fux help WORD` \
         shows it. Generated: FUX_BLESS=1 {TEST}"
    );
    check("words.txt", &file(&head, lines), TEST)
}

/// Each record in `value`, a line `PATH: FIELD…`, its fields in order,
/// once for each distinct path and fields: `path` names where it is (a
/// list's items `[]`); one whose first field is its `event`, `kind` or
/// `status` says which after its path (`[created]`). A held answer or
/// capture, which `get` reads too, lists the fields it has of its kind's
/// table (`record::AnswerField`, …), a capture's state's after them.
fn records(path: &str, value: &Value<Fux>, out: &mut Vec<String>) {
    let mut line = |path: &str, fields: &[&str]| {
        let line = format!("{path}: {}", fields.join(" "));
        if !out.contains(&line) {
            out.push(line);
        }
    };
    let fields: Vec<(String, Value<Fux>)> = match value {
        Value::List(list) => {
            for item in list.items() {
                records(&format!("{path}[]"), item, out);
            }
            return;
        }
        Value::App(Fux::Record(r)) => {
            let which = match r.fields.first() {
                Some((key, Value::Str(text)))
                    if ["event", "kind", "status"].contains(&key.as_str()) =>
                {
                    format!("[{}]", &***text)
                }
                Some(_) | None => String::new(),
            };
            let names: Vec<&str> = r.fields.iter().map(|(key, _)| key.as_str()).collect();
            line(&format!("{path}{which}"), &names);
            r.fields.clone()
        }
        Value::App(Fux::Answer(answer, run)) => {
            let fields: Vec<(String, Value<Fux>)> = held(answer)
                .into_iter()
                .filter_map(|name| {
                    let value = record::answer_field(answer, *run, &name).ok()?;
                    Some((name, value))
                })
                .collect();
            let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
            line(path, &names);
            fields
        }
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::App(_) => return,
    };
    for (key, value) in &fields {
        records(&format!("{path}.{key}"), value, out);
    }
}

/// The fields `get` may read of a held answer or capture: its kind's
/// tables, then a captured pane's state's.
fn held(answer: &super::Answer) -> Vec<String> {
    use record::{AnswerField, CaptureField, CapturedField};
    fn names<T>(all: &[(T, &str)]) -> Vec<String> {
        all.iter().map(|(_, name)| (*name).to_owned()).collect()
    }
    match answer {
        super::Answer::Pane(_) => names(AnswerField::ALL),
        super::Answer::Capture(c) => {
            let mut fields: Vec<String> = names(CaptureField::ALL);
            fields.extend(names(CapturedField::ALL));
            if let fux_wire::answer::Of::Pane { state, .. } = &c.of {
                fields.extend(state.iter().map(|(name, _)| name.clone()));
            }
            fields
        }
        super::Answer::Panes(_) => Vec::new(),
    }
}

/// docs/records.txt lists every record a program reads, each with its
/// fields in order, sorted by where it is, each made by its own builder:
/// - a session's snapshots in each projection, its client having told
///   its terminal's facts (sixel, its cell's size) and its pane shown a
///   picture, styled text and a copy, a pane split with a variable set,
///   and each kind of directory a launch observes;
/// - a capture of that pane, with styles, and of the client;
/// - held answers of every kind: an action's, a wait's that saw a change,
///   one that saw its program end, one whose pane closed, one timed out;
/// - `buffers`, `status`, and a batch of every kind of event.
#[test]
fn records_list_is_current() -> Result<(), Box<dyn std::error::Error>> {
    use super::testing::{InProcess, attached, evaluate};
    use fux_wire::answer::{Change, Ended, PaneAnswer};
    use fux_wire::inspect::{Directory, Path, Projection, Scope};
    let (mut session, client) = attached(24, 80)?;
    let now = std::time::Instant::now();
    let replies = [
        fux_vt::keys::decode::Reply::Attributes { sixel: true },
        fux_vt::keys::decode::Reply::CellSize {
            width: 10,
            height: 20,
        },
        fux_vt::keys::decode::Reply::UnderlineStyles,
    ];
    let acts = replies.map(fux_wire::act::Act::Reply).to_vec();
    session.acts(client, acts, now);
    // Every style there is, a picture on a line of its own (so no text
    // covers it), a copy (OSC 52) and a title.
    let shown = "\x1b[1;2;3;4;5;7;8;9;38;5;1;48;2;1;2;3mstyled\x1b[0m\r\n\
                 \x1bP9q#1;2;100;0;0!20~-!20~\x1b\\\r\n\
                 \x1b]52;c;aGVsbG8=\x07\x1b]2;title\x07";
    let pane = fux_wire::id::PaneId::of(1);
    session.output(pane, shown.as_bytes(), now);
    let mut draft = super::default_state()?.draft();
    let dir = fux_wire::words::quote(env!("CARGO_MANIFEST_DIR"));
    let split = format!("%1 str vertical axis str {dir} cwd split");
    let asks = &mut InProcess(&mut session);
    evaluate(&split, Some(&mut draft), asks, &mut Vec::new())?;
    let mut lines = Vec::new();
    for (name, projection) in [
        ("topology", Projection::Topology),
        ("facts", Projection::Facts),
        ("recipe", Projection::Recipe),
    ] {
        let asked = fux_wire::protocol::Asked {
            after: None,
            context: fux_wire::protocol::Context::default(),
            request: fux_wire::command::Command::Inspect {
                scope: Scope::All,
                projection,
            }
            .into(),
        };
        let reply = super::Asks::ask(asks, &asked)?;
        let fux_wire::protocol::Response::Returned(fux_wire::protocol::Body::Snapshot(mut s)) =
            reply.response
        else {
            return Err(format!("{name}: {:?}", reply.response).into());
        };
        // Every directory a launch can observe, one pane each.
        let observed = [
            Directory::Known(Path::Bytes(vec![0xff])),
            Directory::Failed(2),
        ];
        let recipes = s.panes.iter_mut().filter_map(|p| p.recipe.as_mut());
        for (recipe, observed) in recipes.zip(observed) {
            recipe.observed_cwd = observed;
        }
        let snapshot = record::snapshot(*s).map_err(|e| e.message)?;
        records(&format!("snapshot.{name}"), &snapshot, &mut lines);
    }
    let held = [
        ("capture[pane]", "%1 styles capture".to_owned()),
        ("capture[client]", format!("str {client} client capture")),
    ];
    for (path, code) in held {
        for value in evaluate(&code, None, asks, &mut Vec::new())? {
            records(path, &value, &mut lines);
        }
    }
    let run = session.run;
    let version = format!("%1:0.0.0.0.0:{run}")
        .parse()
        .map_err(|()| "version")?;
    let changed = |ended| PaneAnswer::Changed {
        version,
        changed: Change::ALL.to_vec(),
        ended,
    };
    for (path, answer) in [
        ("answer[acted]", PaneAnswer::Acted(version)),
        ("answer[changed]", changed(None)),
        ("answer[ended]", changed(Some(Ended::Exited(0)))),
        (
            "answer[closed]",
            PaneAnswer::Closed(fux_wire::id::PaneId::of(1)),
        ),
        (
            "answer[timed_out]",
            PaneAnswer::TimedOut {
                since: version,
                limit: std::time::Duration::from_secs(1),
            },
        ),
    ] {
        let value = super::Answer::Pane(answer).in_run(Some(run));
        records(path, &value, &mut lines);
    }
    let socket = fux_server::socket::checked("/nonexistent/fux.sock", "test")?;
    let probe = crate::client::Probe::Server {
        protocol: fux_wire::protocol::PROTOCOL,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        pid: fuxix::process::Pid::from_raw(1),
    };
    let status = crate::status(&socket, &probe)?;
    records(
        "status",
        &record::from_json(&status).map_err(|e| e.message)?,
        &mut lines,
    );
    let batch = fux_server::journal::every_kind_read(run)?;
    let events = record::events(batch).map_err(|e| e.message)?;
    records("events", &events, &mut lines);
    lines.sort();
    let head = format!(
        "Every record a program reads, and each held answer's and capture's \
         fields `get` reads, by where it is (`[]` a list's items; `[NAME]` its \
         event, kind or status, or which answer), each with its fields in order. \
         Generated: FUX_BLESS=1 {TEST}"
    );
    Ok(check("records.txt", &file(&head, lines), TEST)?)
}
