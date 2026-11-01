//! One bridge between language values and owned screen arguments.
use super::{Fux, Value, record};
use fux_wire::callback::{Code, Data, Open};
use fuxth::Failure;
use std::rc::Rc;

fn runs(v: &Data, of: &mut Option<fux_wire::command::Run>) -> Result<(), Failure> {
    match v {
        Data::Handle(_, r) | Data::Client(_, r) => {
            *of = super::same_run(*of, *r)?;
        }
        Data::List(v) => {
            for v in v {
                runs(v, of)?;
            }
        }
        Data::Record { of: run, fields } => {
            *of = super::same_run(*of, *run)?;
            for (_, v) in fields {
                runs(v, of)?;
            }
        }
        Data::Int(_) | Data::Bool(_) | Data::Text(_) | Data::Null | Data::All | Data::Code(_) => {}
    }
    Ok(())
}

fn fail() -> Failure {
    Failure::new(
        fux_wire::code::Code::BadValue.into(),
        "a callback argument must be an immutable value: scalar, handle, record, null, list or code … end",
    )
}

/// `v` as a code value goes on the wire: the words of `code … end`'s xt,
/// which only it keeps, and only when `code` WORDS `end` makes it again.
pub(super) fn code(v: &Value<Fux>) -> Result<Code, Failure> {
    let Value::Xt(xt) = v else {
        let why = format!("takes code … end, not {}", v.kind());
        return Err(Failure::new(fuxth::Code::Type, why));
    };
    let words = xt.words().ok_or_else(|| match xt.name() {
        "" => Failure::new(
            fux_wire::code::Code::BadValue.into(),
            "this code value can't go as its words: a word other than code or end opened or closed it",
        ),
        name => Failure::new(
            fuxth::Code::Type,
            format!("{name} is a word's xt, with no words to send: code {name} end is one"),
        ),
    })?;
    Ok(Code {
        words: words.iter().map(|t| t.text.to_string()).collect(),
    })
}
/// `v` as a callback's argument goes on the wire: refused if its lists and
/// records nest deeper than the wire carries (`callback::DEPTH`).
pub(super) fn data(v: &Value<Fux>) -> Result<Data, Failure> {
    data_within(v, fux_wire::callback::DEPTH.saturating_add(1))
}

fn data_within(v: &Value<Fux>, depth: u8) -> Result<Data, Failure> {
    let depth = depth.checked_sub(1).ok_or_else(|| {
        let why = "a callback argument nests deeper than the wire carries";
        Failure::new(
            fux_wire::code::Code::from(fux_wire::code::Raise::TooLong).into(),
            why,
        )
    })?;
    let data = |v| data_within(v, depth);
    Ok(match v {
        Value::Int(v) => Data::Int(*v),
        Value::Bool(v) => Data::Bool(*v),
        Value::Str(v) => Data::Text(v.to_string()),
        Value::List(v) => Data::List(v.items().iter().map(data).collect::<Result<_, _>>()?),
        Value::App(Fux::Record(r)) => {
            let fields = r.fields.iter().map(|(key, v)| Ok((key.clone(), data(v)?)));
            Data::Record {
                of: r.of,
                fields: fields.collect::<Result<_, Failure>>()?,
            }
        }
        Value::App(Fux::All) => Data::All,
        Value::App(Fux::Null) => Data::Null,
        Value::App(Fux::Pane(p, run)) => Data::Handle(fux_wire::command::AnyRef::Pane(*p), *run),
        Value::App(Fux::Tab(t, run)) => Data::Handle(fux_wire::command::AnyRef::Tab(*t), *run),
        Value::App(Fux::Workspace(w, run)) => {
            Data::Handle(fux_wire::command::AnyRef::Workspace(*w), *run)
        }
        Value::App(Fux::Client(c, run)) => Data::Client(*c, *run),
        Value::Xt(_) => Data::Code(code(v)?),
        Value::App(Fux::Opts(_) | Fux::Extent(_) | Fux::Answer(..))
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_) => {
            return Err(fail());
        }
    })
}

/// `v` as a value of the receiving machine's, each code value in it
/// compiled there by `compile`.
pub(super) fn value(
    v: &Data,
    compile: &mut dyn FnMut(&Code) -> Result<Value<Fux>, super::Failed>,
) -> Result<Value<Fux>, super::Failed> {
    Ok(match v {
        Data::Int(v) => Value::Int(*v),
        Data::Bool(v) => Value::Bool(*v),
        Data::Text(v) => Value::str(v.as_str()),
        Data::List(v) => {
            let out = v.iter().map(|v| value(v, compile));
            Value::List(Rc::new(fuxth::List::new(out.collect::<Result<_, _>>()?)))
        }
        Data::Record {
            of,
            fields: entries,
        } => {
            let fields = (entries.iter()).map(|(key, v)| Ok((key.clone(), value(v, compile)?)));
            record::from_fields(fields.collect::<Result<_, super::Failed>>()?, *of)
                .map_err(super::callback_failed)?
        }
        Data::All => Value::App(Fux::All),
        Data::Null => Value::App(Fux::Null),
        Data::Handle(id, run) => Value::App(super::handle(*id, *run)),
        Data::Client(c, run) => Value::App(Fux::Client(*c, *run)),
        Data::Code(code) => compile(code)?,
    })
}

/// The receiving dictionary checks actual arguments before publishing a
/// screen: its callback's captured values of its server's run, its code
/// values compiled there (stopping at `stop`), its values read.
pub fn admit(
    mut open: Open,
    state: Option<&crate::client_state::Draft>,
    context: &fux_wire::protocol::Context,
    stop: Option<&super::Stop>,
) -> Result<crate::screens::Screen, super::Failed> {
    let refused = |why| super::Failed::new(fux_wire::code::Code::BadValue, why);
    let config = state.and_then(|s| s.config.as_ref());
    let check = |action: &fux_wire::callback::Callback, given: &[&Data]| {
        let mut of = context.of;
        for arg in &action.args {
            runs(arg, &mut of).map_err(super::callback_failed)?;
        }
        let mut machine = super::base(config)?;
        let mut compile = |code: &Code| super::code_value(&mut machine, code, stop);
        let values = action.args.iter().chain(given.iter().copied());
        let read = values.map(|v| value(v, &mut compile).map(drop));
        let checked = (read.collect::<Result<(), _>>()).and_then(|()| compile(&action.code));
        checked.map(drop).map_err(|e| {
            let why = format!("{}, in the callback", e.message);
            super::Failed::new(fux_wire::code::Code::BadValue, why)
        })
    };
    match &mut open {
        Open::List(l) => {
            let Some(client) = context.client else {
                return Err(refused("a list needs its receiving client"));
            };
            l.client = Data::Client(client, context.of);
            if l.current.is_some_and(|n| n >= l.items.len())
                || (!l.items.is_empty() && l.selected >= l.items.len())
                || (l.items.is_empty() && l.selected != 0)
            {
                return Err(refused(
                    "list current or selected index is outside its rows",
                ));
            }
            if let Some((_, run)) = l.about {
                super::same_run(context.of, run).map_err(super::callback_failed)?;
            }
            for item in &mut l.items {
                let mut of = context.of;
                runs(&item.payload, &mut of).map_err(super::callback_failed)?;
                item.label = fux_wire::words::clean(&item.label);
            }
            let payloads: Vec<&Data> = l.items.iter().map(|item| &item.payload).collect();
            check(&l.action, &payloads)?;
            l.title = fux_wire::words::clean(&l.title);
        }
        Open::Prompt(p) => {
            if let Some(action) = &p.action {
                check(action, &[])?;
            }
            p.title = fux_wire::words::clean(&p.title);
        }
    }
    Ok(open.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestResult;
    use crate::screens::{Job, Screen};
    use crate::script::{Asks, Failed, Host};
    use fux_wire::callback::{Item, ListScreen};
    use fux_wire::protocol::{Asked, Context, Reply};

    struct NoRequests;
    impl Asks for NoRequests {
        fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
            Err(Failed::new(
                fux_wire::code::Code::Client,
                "unexpected server effect",
            ))
        }
    }
    fn context() -> Result<Context, String> {
        Ok(Context {
            client: Some("c1".parse().map_err(|e| format!("{e}"))?),
            of: Some("1-2".parse().map_err(|()| "run")?),
            ..Default::default()
        })
    }
    fn list(payload: Data, code: &str) -> Open {
        Open::List(ListScreen {
            title: "title\x1b[31m".into(),
            items: vec![Item {
                payload,
                label: "str injected kill\nlabel".into(),
            }],
            current: None,
            selected: 0,
            about: None,
            action: crate::testing::code(code).into(),
            // Receiving admission supplies its actual client.
            client: Data::Null,
        })
    }
    /// Data with no code value in it needs no machine to read.
    fn no_code(_: &Code) -> Result<Value<Fux>, Failed> {
        Err(Failed::new(
            fux_wire::code::Code::Client,
            "no code value here",
        ))
    }
    fn run(job: &crate::screens::Job) -> Result<String, Failed> {
        let mut machine = job.machine(None, None)?;
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        crate::script::run(&mut machine, &mut host, &mut NoRequests, None)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn callbacks_use_the_receiving_dictionary_and_preserve_nested_typed_data() -> TestResult {
        let context = context()?;
        let handle = Data::Handle("%3".parse().map_err(|()| "pane")?, context.of);
        let payload = Data::Record {
            of: context.of,
            fields: vec![(
                "value".into(),
                Data::List(vec![
                    handle,
                    Data::Null,
                    Data::Bool(true),
                    Data::Int(-3),
                    Data::Text("' drop %1 kill".into()),
                ]),
            )],
        };
        let mut state = crate::script::default_state()?.draft();
        state.config = Some(std::sync::Arc::new(crate::script::source::Frozen::text(
            "receiver",
            "define receiver-only nip str value get json print end",
        )));
        let spec = list(payload, "receiver-only");
        use fux_wire::protocol::{Decoder, Frame, ToAttached};
        let encoded = ToAttached::Open(spec).encode();
        let mut decoder = Decoder::default();
        decoder.push(&encoded);
        let Some(ToAttached::Open(spec)) = decoder.frame::<ToAttached>()? else {
            return Err("typed screen relay".into());
        };
        let Screen::List(screen) = admit(spec, Some(&state), &context, None)? else {
            return Err("list".into());
        };
        assert!(
            !screen
                .items
                .iter()
                .any(|i| i.label.contains(char::is_control))
        );
        let item = screen.items.first().ok_or("row")?;
        let value = value(&item.payload, &mut no_code).map_err(|e| e.message)?;
        let Value::App(Fux::Record(record)) = value else {
            return Err("record".into());
        };
        let Value::List(values) = record.get("value").map_err(|e| e.message)? else {
            return Err("values".into());
        };
        assert_eq!(
            values.items().first(),
            Some(&Value::App(Fux::Pane(
                fux_wire::id::PaneId::of(3),
                context.of
            )))
        );
        assert_eq!(values.items().get(1), Some(&Value::App(Fux::Null)));
        let job = Job::call(&screen.action, [screen.client, item.payload.clone()]);
        let mut machine = job.machine(state.config.as_ref(), None)?;
        let mut out = Vec::new();
        let mut host = Host::new(context, None, &mut out);
        crate::script::run(&mut machine, &mut host, &mut NoRequests, None)?;
        assert!(String::from_utf8_lossy(&out).contains("[\"%3\",null,true,-3,\"' drop %1 kill\"]"));
        Ok(())
    }

    #[test]
    fn callback_snapshots_keep_all_scope_and_children_after_the_source_closes() -> TestResult {
        let (mut session, _) = crate::script::testing::attached(8, 30)?;
        let reply = crate::script::testing::InProcess(&mut session).ask(&Asked {
            after: None,
            context: Context::default(),
            request: fux_wire::command::Command::Inspect {
                scope: fux_wire::inspect::Scope::All,
                projection: fux_wire::inspect::Projection::Topology,
            }
            .into(),
        })?;
        let mut values = crate::script::answered(reply)?;
        let root = values.pop().ok_or("snapshot")?;
        let payload = data(&root).map_err(|e| e.message)?;
        let bytes = fux_wire::wire::encode(&payload);
        let payload: Data = fux_wire::wire::Reader::new(&bytes).whole()?;
        let restored = value(&payload, &mut no_code).map_err(|e| e.message)?;
        assert_eq!(data(&restored).map_err(|e| e.message)?, payload);
        assert_eq!(restored, root);
        let Value::App(Fux::Record(record)) = restored else {
            return Err("record".into());
        };
        assert_eq!(record.of, Some(session.run));
        assert_eq!(
            record.get("scope").map_err(|e| e.message)?,
            Value::App(Fux::All)
        );
        let Value::List(panes) = record.get("panes").map_err(|e| e.message)? else {
            return Err("panes".into());
        };
        let child = panes.items().first().ok_or("pane")?.clone();
        drop(panes);
        drop(record);
        drop(root);
        crate::script::testing::evaluate(
            "%1 kill",
            None,
            &mut crate::script::testing::InProcess(&mut session),
            &mut Vec::new(),
        )?;
        let context = Context {
            of: Some(session.run),
            ..context()?
        };
        let Screen::List(screen) = admit(
            list(payload, "nip str scope get kind print"),
            None,
            &context,
            None,
        )?
        else {
            return Err("list".into());
        };
        let payload = screen.items.first().ok_or("row")?.payload.clone();
        assert_eq!(
            run(&Job::call(&screen.action, [screen.client, payload]))?,
            "all\n"
        );
        let child = data(&child).map_err(|e| e.message)?;
        let text = run(&Job::call(
            &crate::testing::code("str id get kind print").into(),
            [child],
        ))?;
        assert_eq!(text, "pane\n");
        Ok(())
    }

    #[test]
    fn callbacks_check_empty_snapshot_provenance_and_mixed_nested_handles() -> TestResult {
        let context = context()?;
        let run = context.of.ok_or("run")?;
        let snapshot = fux_wire::inspect::Snapshot {
            run,
            cursor: format!("0:{run}").parse().map_err(|()| "cursor")?,
            scope: fux_wire::inspect::Scope::All,
            projection: fux_wire::inspect::Projection::Topology,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            clients: Vec::new(),
        };
        let record = record::snapshot(snapshot).map_err(|e| e.message)?;
        let original = data(&record).map_err(|e| e.message)?;
        let bytes = fux_wire::wire::encode(&original);
        let decoded = fux_wire::wire::Reader::new(&bytes).whole()?;
        let restored = value(&decoded, &mut no_code).map_err(|e| e.message)?;
        let payload = data(&restored).map_err(|e| e.message)?;
        assert_eq!(payload, original);
        assert!(admit(list(payload.clone(), "2drop"), None, &context, None).is_ok());
        let other = "3-4".parse().map_err(|()| "run")?;
        let stale = Context {
            of: Some(other),
            ..context.clone()
        };
        assert!(admit(list(payload.clone(), "2drop"), None, &stale, None).is_err());
        let mut inferred = None;
        runs(&payload, &mut inferred).map_err(|e| e.message)?;
        assert_eq!(inferred, Some(run));
        let nested = Data::Record {
            of: None,
            fields: vec![(
                "nested".into(),
                Data::List(vec![
                    payload,
                    Data::Handle(
                        fux_wire::command::AnyRef::Pane(fux_wire::id::PaneId::of(7)),
                        Some(run),
                    ),
                    Data::Client(fux_wire::id::ClientId::of(2), Some(run)),
                    Data::All,
                ]),
            )],
        };
        assert!(admit(list(nested.clone(), "2drop"), None, &context, None).is_ok());
        assert!(admit(list(nested.clone(), "2drop"), None, &stale, None).is_err());
        let mixed = Data::List(vec![
            nested,
            Data::Client(fux_wire::id::ClientId::of(2), Some(other)),
        ]);
        assert!(
            admit(
                list(mixed, "2drop"),
                None,
                &Context {
                    of: None,
                    ..context
                },
                None
            )
            .is_err()
        );
        Ok(())
    }

    /// A callback is admitted once its code compiles, and its words check
    /// their actual arguments as it runs, as does its end: it leaves
    /// nothing.
    #[test]
    fn callbacks_fail_on_actual_kinds_and_values_left_as_they_run() -> TestResult {
        let context = context()?;
        for (payload, code) in [
            (Data::Text("host".into()), "1 roll drop 1 add drop"),
            (Data::Int(1), "2drop 7"),
        ] {
            assert!(
                admit(list(payload.clone(), code), None, &context, None).is_ok(),
                "{code}"
            );
            let job = Job::call(&crate::testing::code(code).into(), [Data::Null, payload]);
            assert!(run(&job).is_err(), "{code}");
        }
        let callback = crate::testing::code("nip first 1 add drop").into();
        let job = Job::call(
            &callback,
            [Data::Null, Data::List(vec![Data::Text("inert".into())])],
        );
        assert!(job.machine(None, None).is_ok());
        assert!(run(&job).is_err());
        let job = Job::call(
            &crate::testing::code("2drop list 7 end each end").into(),
            [Data::Null, Data::Int(1)],
        );
        assert!(job.machine(None, None).is_ok());
        assert!(run(&job).is_err_and(|e| e.message.contains("empty stack")));
        Ok(())
    }

    #[test]
    fn screen_admission_checks_every_payload_run_and_aggregate_bound() -> TestResult {
        let context = context()?;
        let mut spec = list(
            Data::Handle("%1".parse().map_err(|()| "pane")?, context.of),
            "2drop",
        );
        let Open::List(screen) = &mut spec else {
            return Err("list".into());
        };
        screen.items.push(Item {
            label: "same kind, another run".into(),
            payload: Data::Handle(
                "%1".parse().map_err(|()| "pane")?,
                Some("3-4".parse().map_err(|()| "run")?),
            ),
        });
        assert!(admit(spec, None, &context, None).is_err());
        assert!(data(&Value::App(Fux::Opts(std::rc::Rc::new(Default::default())))).is_err());
        Ok(())
    }

    /// A prompt's captured values are of its client's server run, as a
    /// list's are, however deep.
    #[test]
    fn prompt_admission_checks_its_captured_values_run() -> TestResult {
        let context = context()?;
        let pane = |run: &str| -> Result<Data, String> {
            let run = Some(run.parse().map_err(|()| "run")?);
            Ok(Data::Handle("%1".parse().map_err(|()| "pane")?, run))
        };
        let prompt = |args| {
            Open::Prompt(fux_wire::callback::Prompt {
                title: "t".into(),
                line: fux_wire::act::Line::new(String::new()),
                action: Some(fux_wire::callback::Callback {
                    code: crate::testing::code("2drop"),
                    args,
                }),
            })
        };
        assert!(admit(prompt(vec![pane("1-2")?]), None, &context, None).is_ok());
        for args in [vec![pane("3-4")?], vec![Data::List(vec![pane("3-4")?])]] {
            let refused = admit(prompt(args), None, &context, None).err();
            assert!(refused.is_some_and(|e| e.message.contains("different server runs")));
        }
        Ok(())
    }

    /// A code value is compiled as a key's program runs: an `include` in
    /// it reads no file, though the file is there; and words that close
    /// more than they open compile to nothing.
    #[test]
    fn a_code_value_compiles_as_a_keys_program_runs() -> TestResult {
        let dir = fuxix::file::scratch()?.join(format!("callbacks-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("action.fux");
        std::fs::write(&file, "str read")?;
        let words = vec!["include".into(), file.display().to_string(), "drop".into()];
        let job = Job::from(Code { words });
        let refused = job.machine(None, None).err().map(|e| e.to_string());
        std::fs::remove_dir_all(&dir)?;
        assert!(
            refused
                .as_ref()
                .is_some_and(|e| e.contains("reads no files")),
            "{refused:?}"
        );
        let refused = Job::from(crate::testing::code("end 1 code")).machine(None, None);
        assert!(refused.is_err_and(|e| e.message.contains("close what they did not open")));
        Ok(())
    }
}
