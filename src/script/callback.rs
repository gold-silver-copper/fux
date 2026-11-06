//! Callbacks keep an explicit form and inert captured values across clients.
use super::{Failed, Failure, Fux, List, Value};
use fux_wire::callback::{Code, Data, Open};
use std::rc::Rc;
pub(super) fn code(v: &Value<Fux>) -> Result<Code, Failure> {
    if let Value::App(Fux::Code(c)) = v {
        Ok((**c).clone())
    } else {
        Err(Failure::new(
            fux_wire::code::Code::Type,
            "callback needs (code! (lambda! (...) ...))",
        ))
    }
}
pub(super) fn data(v: &Value<Fux>) -> Result<Data, Failure> {
    within(v, fux_wire::callback::DEPTH.saturating_add(1))
}
fn within(v: &Value<Fux>, depth: u8) -> Result<Data, Failure> {
    let next = depth.checked_sub(1).ok_or_else(|| {
        Failure::new(
            fux_wire::code::Code::TooLong,
            "callback data is too deeply nested",
        )
    })?;
    Ok(match v {
        Value::Float(n) => Data::FloatBits(n.to_bits()),
        Value::Bytes(bytes) => Data::Bytes((**bytes).clone()),
        Value::Int(n) => Data::Int(*n),
        Value::Bool(b) => Data::Bool(*b),
        Value::Str(s) => Data::Text(s.to_string()),
        Value::Symbol(s) => Data::Symbol(s.to_string()),
        Value::List(xs) => Data::List(
            xs.items()
                .iter()
                .map(|v| within(v, next))
                .collect::<Result<_, _>>()?,
        ),
        Value::App(Fux::Null) => Data::Null,
        Value::App(Fux::All) => Data::All,
        Value::App(Fux::Pane(p, r)) => Data::Handle(fux_wire::command::AnyRef::Pane(*p), *r),
        Value::App(Fux::Tab(p, r)) => Data::Handle(fux_wire::command::AnyRef::Tab(*p), *r),
        Value::App(Fux::Workspace(p, r)) => {
            Data::Handle(fux_wire::command::AnyRef::Workspace(*p), *r)
        }
        Value::App(Fux::Client(p, r)) => Data::Client(*p, *r),
        Value::App(Fux::Record(r)) => Data::Record {
            of: r.of,
            fields: r
                .fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), within(v, next)?)))
                .collect::<Result<_, Failure>>()?,
        },
        Value::App(Fux::Code(c)) => Data::Code((**c).clone()),
        Value::App(_) | Value::Printed(_) => {
            return Err(Failure::new(
                fux_wire::code::Code::BadValue,
                "callback arguments must be immutable data",
            ));
        }
    })
}
pub(super) fn value(v: &Data) -> Result<Value<Fux>, Failed> {
    Ok(match v {
        Data::FloatBits(bits) => Value::Float(f64::from_bits(*bits)),
        Data::Bytes(bytes) => Value::Bytes(Rc::new(bytes.clone())),
        Data::Int(n) => Value::Int(*n),
        Data::Bool(b) => Value::Bool(*b),
        Data::Text(s) => Value::str(s),
        Data::Symbol(s) => Value::Symbol(Rc::new(s.clone())),
        Data::List(xs) => Value::List(Rc::new(List::new(
            xs.iter().map(value).collect::<Result<_, _>>()?,
        ))),
        Data::Null => Value::App(Fux::Null),
        Data::All => Value::App(Fux::All),
        Data::Handle(h, r) => Value::App(super::handle(*h, *r)),
        Data::Client(c, r) => Value::App(Fux::Client(*c, *r)),
        Data::Code(c) => Value::App(Fux::Code(Rc::new(c.clone()))),
        Data::Record { of, fields } => super::record::from_fields(
            fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), value(v)?)))
                .collect::<Result<_, Failed>>()?,
            *of,
        )
        .map_err(super::callback_failed)?,
    })
}
fn runs(v: &Data, of: &mut Option<fux_wire::command::Run>) -> Result<(), Failed> {
    match v {
        Data::Handle(_, r) | Data::Client(_, r) => {
            *of = super::same_run(*of, *r).map_err(super::callback_failed)?
        }
        Data::Record { of: r, fields } => {
            *of = super::same_run(*of, *r).map_err(super::callback_failed)?;
            for (_, v) in fields {
                runs(v, of)?;
            }
        }
        Data::List(xs) => {
            for v in xs {
                runs(v, of)?;
            }
        }
        Data::Int(_)
        | Data::Bool(_)
        | Data::Text(_)
        | Data::All
        | Data::Null
        | Data::Code(_)
        | Data::Bytes(_)
        | Data::FloatBits(_)
        | Data::Symbol(_) => {}
    }
    Ok(())
}
pub fn admit(
    mut open: Open,
    _state: Option<&crate::client_state::Draft>,
    context: &fux_wire::protocol::Context,
    stop: Option<&super::Stop>,
) -> Result<crate::screens::Screen, Failed> {
    if let Some(stop) = stop {
        stop.check()?;
    }
    let check = |action: &fux_wire::callback::Callback| {
        let mut of = context.of;
        for arg in &action.args {
            runs(arg, &mut of)?;
        }
        // Read the whole expression as data, without executing user code.
        let mut g: grift::Grift = grift::Grift::new()
            .map_err(|_| Failed::new(fux_wire::code::Code::Memory, "initializing grift"))?;
        let parsed = g.read(&action.code.source);
        match parsed {
            Ok(_) => Ok(()),
            Err(e) => {
                let mut text = String::new();
                let _ = g.describe(&e, &mut text);
                Err(Failed::new(fux_wire::code::Code::BadValue, text))
            }
        }
    };
    match &mut open {
        Open::List(l) => {
            let client = context.client.ok_or_else(|| {
                Failed::new(
                    fux_wire::code::Code::BadValue,
                    "list needs its receiving client",
                )
            })?;
            l.client = Data::Client(client, context.of);
            if l.current.is_some_and(|n| n >= l.items.len())
                || (!l.items.is_empty() && l.selected >= l.items.len())
                || (l.items.is_empty() && l.selected != 0)
            {
                return Err(Failed::new(
                    fux_wire::code::Code::BadValue,
                    "list selection is outside its rows",
                ));
            }
            if let Some((_, run)) = l.about {
                super::same_run(context.of, run).map_err(super::callback_failed)?;
            }
            for item in &mut l.items {
                let mut of = context.of;
                runs(&item.payload, &mut of)?;
                item.label = fux_wire::words::clean(&item.label);
            }
            check(&l.action)?;
            l.title = fux_wire::words::clean(&l.title);
        }
        Open::Prompt(p) => {
            if let Some(action) = &p.action {
                check(action)?;
            }
            p.title = fux_wire::words::clean(&p.title);
        }
    }
    Ok(open.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{AsClient, Asks, Host};
    use fux_wire::protocol::{Asked, Context, Reply};
    struct NoRequests;
    impl Asks for NoRequests {
        fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
            Err(Failed::new(
                fux_wire::code::Code::Client,
                "unexpected server request",
            ))
        }
    }
    #[test]
    fn callbacks_use_the_receiving_dictionary_and_preserve_nested_typed_data() -> crate::TestResult
    {
        let run = "1-2".parse().map_err(|()| "run")?;
        let client = "c1".parse().map_err(|e| format!("{e}"))?;
        let payload = Data::Record {
            of: Some(run),
            fields: vec![(
                "value".into(),
                Data::List(vec![
                    Data::Handle("%3".parse().map_err(|()| "id")?, Some(run)),
                    Data::Null,
                    Data::List(Vec::new()),
                    Data::Bool(true),
                    Data::Int(-3),
                    Data::Text("(kill (all))".into()),
                    Data::Bytes(vec![0, 255, 3]),
                    Data::FloatBits(1.5f64.to_bits()),
                ]),
            )],
        };
        let mut state = crate::script::default_state()?.draft();
        state.config = Some(std::sync::Arc::new(crate::script::source::Frozen::text(
            "receiver.grift",
            "(define! receiver-only (lambda! (client payload) (display (get payload \"value\"))))",
        )));
        let code = Code {
            source: "(lambda! (client payload) (receiver-only client payload))".into(),
        };
        let job =
            crate::screens::Job::call(&code.into(), [Data::Client(client, Some(run)), payload]);
        let mut machine = job.machine(state.config.as_ref(), None)?;
        let mut out = Vec::new();
        let context = Context {
            client: Some(client),
            of: Some(run),
            ..Default::default()
        };
        let mut host = Host::job(context, &mut state, &mut out);
        crate::script::run(&mut machine, &mut host, &mut NoRequests, None)?;
        let text = String::from_utf8(out)?;
        assert!(text.contains("none () #true -3"), "{text}");
        assert!(text.contains("#bytes(0 255 3) 1.5"), "{text}");
        assert!(text.contains("\"(kill (all))\""), "{text}");
        Ok(())
    }
    #[test]
    fn callback_admission_retains_absence_empty_lists_bytes_and_float_bits() -> crate::TestResult {
        let payload = Data::List(vec![
            Data::Null,
            Data::List(Vec::new()),
            Data::Symbol("east".into()),
            Data::Bytes(vec![0, 255]),
            Data::FloatBits((-0.0f64).to_bits()),
        ]);
        let code=Code{source:"(lambda! (payload) (choose (me) \"typed data\" (list (list payload \"Data\")) () (list 0) (code! (lambda! (client payload) #inert))))".into()};
        let job = crate::screens::Job::call(&code.into(), [payload.clone()]);
        let mut machine = job.machine(None, None)?;
        let mut state = crate::script::default_state()?.draft();
        let mut out = Vec::new();
        let mut transport = NoRequests;
        let context = Context {
            client: Some("c1".parse().map_err(|e| format!("{e}"))?),
            ..Default::default()
        };
        let mut asks = AsClient {
            asks: &mut transport,
            context: context.clone(),
            opened: None,
            on_commit: None,
            stop: None,
        };
        let mut host = Host::job(context, &mut state, &mut out);
        crate::script::run(&mut machine, &mut host, &mut asks, None)?;
        let Some(crate::screens::Screen::List(screen)) = asks.opened else {
            return Err("list not opened".into());
        };
        assert_eq!(screen.items.first().ok_or("row")?.payload, payload);
        let encoded = fux_wire::wire::encode(&screen.items.first().ok_or("row")?.payload);
        let decoded: Data = fux_wire::wire::Reader::new(&encoded).whole()?;
        assert_eq!(decoded, payload);
        Ok(())
    }
    use crate::TestResult;
    use crate::screens::{Job, Screen};
    use crate::script::record;
    use fux_wire::callback::{Item, ListScreen};
    fn run(job: &Job) -> Result<String, Failed> {
        let mut machine = job.machine(None, None)?;
        let mut state = crate::script::default_state()?.draft();
        let mut out = Vec::new();
        let mut host = Host::job(
            context().map_err(|e| Failed::new(fux_wire::code::Code::BadValue, e))?,
            &mut state,
            &mut out,
        );
        crate::script::run(&mut machine, &mut host, &mut NoRequests, None)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
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
        let restored = value(&decoded).map_err(|e| e.message)?;
        let payload = data(&restored).map_err(|e| e.message)?;
        assert_eq!(payload, original);
        assert!(
            admit(
                list(payload.clone(), "(lambda! (#ignore #ignore) #inert)"),
                None,
                &context,
                None
            )
            .is_ok()
        );
        let other = "3-4".parse().map_err(|()| "run")?;
        let stale = Context {
            of: Some(other),
            ..context.clone()
        };
        assert!(
            admit(
                list(payload.clone(), "(lambda! (#ignore #ignore) #inert)"),
                None,
                &stale,
                None
            )
            .is_err()
        );
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
        assert!(
            admit(
                list(nested.clone(), "(lambda! (#ignore #ignore) #inert)"),
                None,
                &context,
                None
            )
            .is_ok()
        );
        assert!(
            admit(
                list(nested.clone(), "(lambda! (#ignore #ignore) #inert)"),
                None,
                &stale,
                None
            )
            .is_err()
        );
        let mixed = Data::List(vec![
            nested,
            Data::Client(fux_wire::id::ClientId::of(2), Some(other)),
        ]);
        assert!(
            admit(
                list(mixed, "(lambda! (#ignore #ignore) #inert)"),
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
    #[test]
    fn screen_admission_checks_every_payload_run_and_aggregate_bound() -> TestResult {
        let context = context()?;
        let mut spec = list(
            Data::Handle("%1".parse().map_err(|()| "pane")?, context.of),
            "(lambda! (#ignore #ignore) #inert)",
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
                    code: crate::testing::code("(lambda! (#ignore #ignore) #inert)"),
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
        let restored = value(&payload).map_err(|e| e.message)?;
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
            "(kill (pane 1))",
            None,
            &mut crate::script::testing::InProcess(&mut session),
            &mut Vec::new(),
        )?;
        let context = Context {
            of: Some(session.run),
            ..context()?
        };
        let Screen::List(screen) = admit(
            list(
                payload,
                "(lambda! (#ignore payload) (print (kind (get payload \"scope\"))))",
            ),
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
            &crate::testing::code("(lambda! (payload) (print (kind (get payload \"id\"))))").into(),
            [child],
        ))?;
        assert_eq!(text, "pane\n");
        Ok(())
    }
    #[test]
    fn a_clients_code_values_are_read_as_their_words() -> TestResult {
        let code = crate::testing::code("(lambda! () (confirm-close (me) (focused (me))))");
        let answer = Data::record([("code", Data::Code(code.clone()))]);
        let reply = fux_wire::protocol::Body::Read(answer.clone()).into();
        let read = crate::script::answered(reply)?;
        let [Value::App(Fux::Record(record))] = read.as_slice() else {
            return Err("a record".into());
        };
        let held = record.get("code").map_err(|e| e.message)?;
        assert_eq!(held, Value::App(Fux::Code(Rc::new(code))));
        let sent = data(&Value::App(Fux::Record(Rc::clone(record))));
        assert_eq!(sent.map_err(|e| e.message)?, answer);
        Ok(())
    }
    #[test]
    fn callbacks_fail_on_actual_kinds_and_values_left_as_they_run() -> TestResult {
        let context = context()?;
        for (payload, source) in [
            (
                Data::Text("host".into()),
                "(lambda! (#ignore payload) (add payload 1))",
            ),
            (Data::Int(1), "(lambda! (#ignore #ignore) (options))"),
        ] {
            assert!(admit(list(payload.clone(), source), None, &context, None).is_ok());
            let job = Job::call(&crate::testing::code(source).into(), [Data::Null, payload]);
            assert!(run(&job).is_err(), "{source}");
        }
        let job = Job::call(
            &crate::testing::code("(lambda! (#ignore payload) (add (first payload) 1))").into(),
            [Data::Null, Data::List(Vec::new())],
        );
        assert!(run(&job).is_err());
        Ok(())
    }
    #[test]
    fn a_code_value_compiles_as_a_keys_program_runs() -> TestResult {
        let dir = fuxix::file::scratch()?.join(format!("callbacks-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("action.grift");
        std::fs::write(&file, "(print \"read\")")?;
        let code = format!(
            "(lambda! () (include! {}))",
            crate::script::quoted(&file.to_string_lossy())
        );
        let job = Job::from(crate::testing::code(&code));
        let prepared = job.machine(None, None);
        assert!(
            prepared.is_ok(),
            "reading the retained form does not run imports"
        );
        let result = run(&job);
        std::fs::remove_dir_all(&dir)?;
        assert!(result.is_err_and(|failure| failure.message.contains("reads no files")));
        Ok(())
    }
    #[test]
    fn a_sent_code_value_compiles_at_execute_as_a_callbacks_does() -> TestResult {
        let job = Job::from(crate::testing::code(
            r#"(lambda! ()
            (define! kept "kept")
            (define! result (execute! (code! (lambda! (a b) (add a b))) 1 2))
            (print (data-text (list result 7 kept))))"#,
        ));
        assert_eq!(run(&job)?, "(3 7 \"kept\")\n");
        let deferred = Job::from(crate::testing::code(
            r#"(lambda! ()
            (define! retained (code! (lambda! () (rename (pane 1) "x"))))
            (print (kind retained)))"#,
        ));
        assert_eq!(run(&deferred)?, "code\n");
        let stop = crate::script::Stop::default();
        let stopper = stop.clone();
        let stopping = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            stopper.stop();
        });
        let mut machine = crate::script::Machine::source(
            "stopped",
            "(execute! (code! (lambda! () (define! spin (lambda! () (spin))) (spin))))",
        );
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        let result = crate::script::run(&mut machine, &mut host, &mut NoRequests, Some(&stop));
        stopping.join().map_err(|_| "stopper")?;
        assert!(result.is_err() && stop.stopped());
        Ok(())
    }
}
