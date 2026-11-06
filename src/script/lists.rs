//! `docs/words.txt` and `docs/records.txt`, written from fux's word table
//! and the libraries' definitions, and from records the record builders
//! make; a test fails while a list is stale (`fux_wire::lists`).
use super::Word;
#[cfg(unix)]
use super::{Fux, Value, record};
use fux_wire::lists::{check, file};

const TEST: &str = "cargo test -p fux --lib lists";

/// docs/words.txt lists every word a fux program may use, each with its
/// stack effect as help shows it.
#[test]
fn words_list_is_current() -> Result<(), String> {
    let fuxs = Word::ALL.iter().map(|w| (w.row().name, w.row().effect));
    let mut lines = vec!["## fux's words".to_owned()];
    lines.extend(fuxs.map(|(name, effect)| format!("{name} ( {effect} )")));
    lines.push("## the libraries' operations, written in grift".to_owned());
    let docs = super::docs().map_err(|e| e.to_string())?;
    lines.extend(docs.iter().map(|d| format!("{} {}", d.name, d.parameters)));
    lines.push("## grift's standard names".to_owned());
    lines.extend(super::language().map_err(|e| e.to_string())?.iter().cloned());
    lines.push("## what fux adds to the language".to_owned());
    lines.extend(super::HOST.iter().map(|(name, _)| (*name).to_owned()));
    let head = format!(
        "Every word a fux program may use, with what it takes and gives as `(explain \"WORD\")` \
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
#[cfg(unix)]
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
        Value::Int(_)
        | Value::Float(_)
        | Value::Bool(_)
        | Value::Str(_)
        | Value::Symbol(_)
        | Value::App(_)
        | Value::Bytes(_) => return,
    };
    for (key, value) in &fields {
        records(&format!("{path}.{key}"), value, out);
    }
}

/// docs/records.txt lists every record a program reads, each with its
/// fields in order, sorted by where it is, each made by its own builder:
/// - a session's snapshots in each projection, its client having told
///   its terminal's facts (sixel, its cell's size) and its pane shown a
///   picture, styled text, a copy, a link and a marked command, a pane split with a variable set,
///   and each kind of directory a launch observes;
/// - a capture of that pane, with styles, since the split, and of the
///   client;
/// - held answers of every kind: an action's, a wait's that saw a change,
///   one that saw its program end, one whose pane closed, one timed out;
/// - `buffers`, `status`, a batch of every kind of event, a word's record
///   and the dictionary's;
/// - the bundled keys' and preferences' records, as a client answers them.
// Unix alone: the list is generated once, and the test names `/` paths.
#[cfg(unix)]
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
    // covers it), a copy (OSC 52), a title, a link (OSC 8) and a marked
    // command (OSC 133).
    let shown = "\x1b[1;2;3;4;5;7;8;9;38;5;1;48;2;1;2;3mstyled\x1b[0m\r\n\
                 \x1bP9q#1;2;100;0;0!20~-!20~\x1b\\\r\n\
                 \x1b]52;c;aGVsbG8=\x07\x1b]2;title\x07\
                 \x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\\r\n\
                 \x1b]133;C\x07out\x1b]133;D;0\x07";
    let pane = fux_wire::id::PaneId::of(1);
    session.output(pane, shown.as_bytes(), now);
    let run = session.run;
    let mut draft = super::default_state()?.draft();
    let dir = super::quoted(env!("CARGO_MANIFEST_DIR"));
    let split = format!(
        "(split (directory (axis (pane 1) \"vertical\") {dir})) (stack (directory (pane 1) {dir}))"
    );
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
        let cwds = s.panes.iter_mut().filter_map(|p| p.cwd.as_mut());
        for (cwd, observed) in cwds.zip(observed) {
            *cwd = observed;
        }
        let snapshot = record::snapshot(*s).map_err(|e| e.message)?;
        records(&format!("snapshot.{name}"), &snapshot, &mut lines);
    }
    // Every style and a link again, written after the split resized the
    // pane, for a capture since then.
    let before = (session.parts.panes().get(&pane))
        .ok_or("pane")?
        .version(run);
    let again = "\x1b[1;2;3;4;5;7;8;9;38;5;1;48;2;1;2;3mstyled\x1b[0m\r\n\
                 \x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\\r\n";
    session.output(pane, again.as_bytes(), now);
    let asks = &mut InProcess(&mut session);
    let held = [
        (
            "capture[pane]",
            format!(
                "(capture (since (styles (pane 1)) {}))",
                super::quoted(&before.to_string())
            ),
        ),
        (
            "capture[client]",
            format!("(capture (client {}))", super::quoted(&client.to_string())),
        ),
    ];
    let held = held.into_iter().chain([
        ("word", "(word \"first\")".to_owned()),
        ("dictionary", "(dictionary)".to_owned()),
    ]);
    for (path, code) in held {
        for value in evaluate(&code, None, asks, &mut Vec::new())? {
            records(path, &value, &mut lines);
        }
    }
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
        let value = super::Answer::Pane(answer).in_run(Some(run))?;
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
    // What a client answers of its own, as a reader gets it: every kind of
    // key it binds, and its preferences.
    let state = super::default_state()?;
    let own = [
        ("keys", state.keys.record(&[])),
        ("preferences", state.preferences.record()),
    ];
    for (path, data) in own {
        let reply = fux_wire::protocol::Body::Read(data).into();
        for value in super::answered(reply)? {
            records(path, &value, &mut lines);
        }
    }
    let batch = fux_server::journal::every_kind_read(run)?;
    let events = record::events(batch).map_err(|e| e.message)?;
    records("events", &events, &mut lines);
    lines.sort();
    let head = format!(
        "Every record a program reads, and each held answer's and capture's \
         fields `get` reads, by where it is (`[]` a list's items; `[NAME]` its \
         event, kind or status, or which answer), each with its fields in order. \
         A word's `codes` is the codes it can fail with, or none where they are unknown. \
         Generated: FUX_BLESS=1 {TEST}"
    );
    Ok(check("records.txt", &file(&head, lines), TEST)?)
}
