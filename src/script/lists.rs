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
/// list's items `[]`); one whose first field is its `event` or `kind`
/// says which after its path (`[created]`).
fn records(path: &str, value: &Value<Fux>, out: &mut Vec<String>) {
    match value {
        Value::List(list) => {
            for item in list.items() {
                records(&format!("{path}[]"), item, out);
            }
        }
        Value::App(Fux::Record(r)) => {
            let which = match r.fields.first() {
                Some((key, Value::Str(text))) if key == "event" || key == "kind" => {
                    format!("[{}]", &***text)
                }
                Some(_) | None => String::new(),
            };
            let fields: Vec<&str> = r.fields.iter().map(|(key, _)| key.as_str()).collect();
            let line = format!("{path}{which}: {}", fields.join(" "));
            if !out.contains(&line) {
                out.push(line);
            }
            for (key, value) in &r.fields {
                records(&format!("{path}.{key}"), value, out);
            }
        }
        Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::App(_) => {}
    }
}

/// docs/records.txt lists every record kind a program gets, each with its
/// fields in order, sorted by where it is: a session's snapshots, in each
/// projection, of a pane split with a variable set, and a batch of every
/// kind of event. A record a field holds only where this session has none
/// (a pane's `cell`, its `pictures`, a launch's `observed_cwd`) shows as
/// that field alone.
#[test]
fn records_list_is_current() -> Result<(), Box<dyn std::error::Error>> {
    use super::testing::{InProcess, attached, evaluate};
    let (mut session, _) = attached(24, 80)?;
    let mut draft = super::default_state()?.draft();
    let dir = env!("CARGO_MANIFEST_DIR");
    let split = format!("%1 str vertical axis str NAME str value env str {dir} cwd split");
    evaluate(
        &split,
        Some(&mut draft),
        &mut InProcess(&mut session),
        &mut Vec::new(),
    )?;
    let mut lines = Vec::new();
    for projection in ["topology", "facts", "recipe"] {
        let code = format!("all str {projection} snapshot");
        let values = evaluate(&code, None, &mut InProcess(&mut session), &mut Vec::new())?;
        for value in &values {
            records(&format!("snapshot.{projection}"), value, &mut lines);
        }
    }
    let batch = fux_server::journal::every_kind_read(session.run)?;
    let imports = fuxth::Imports::new(fux_wire::inspect::BYTES);
    let events = record::events(batch, &imports).map_err(|e| e.message)?;
    records("events", &events, &mut lines);
    lines.sort();
    let head = format!(
        "Every record a program gets, by where it is (`[]` a list's items; \
         `[NAME]` its event or kind), each with its fields in order. Generated: \
         FUX_BLESS=1 {TEST}"
    );
    Ok(check("records.txt", &file(&head, lines), TEST)?)
}
