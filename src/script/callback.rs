//! One bounded bridge between language values and owned screen arguments.
use super::{Fux, Value, record};
use fux_wire::callback::{Data, Open};
use fux_wire::wire::Budget;
use fuxix::Allocation;
use fuxth::Failure;

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
        Data::Int(_) | Data::Bool(_) | Data::Text(_) | Data::Null | Data::All => {}
    }
    Ok(())
}

fn fail() -> Failure {
    Failure::new(
        fux_wire::code::Code::BadValue.into(),
        "a callback argument must be an immutable value: scalar, handle, record, null or list",
    )
}
pub(super) fn data(v: &Value<Fux>, b: &mut Budget) -> Result<Data, Failure> {
    if v.depth() > fuxth::NESTING || v.weight() > fuxth::TOTAL {
        return Err(fail());
    }
    b.slots::<Data>(1)?;
    Ok(match v {
        Value::Int(v) => Data::Int(*v),
        Value::Bool(v) => Data::Bool(*v),
        Value::Str(v) => Data::Text(b.string(v.as_ref())?),
        Value::List(v) => {
            let mut out = b.vector(v.items().len())?;
            for v in v.items() {
                out.push(data(v, b)?);
            }
            Data::List(out)
        }
        Value::App(Fux::Record(r)) => {
            let mut fields = b.vector(r.fields.len())?;
            for (key, v) in &r.fields {
                fields.push((b.string(key)?, data(v, b)?));
            }
            Data::Record { of: r.of, fields }
        }
        Value::App(Fux::All) => Data::All,
        Value::App(Fux::Null) => Data::Null,
        Value::App(Fux::Pane(p, run)) => Data::Handle(fux_wire::command::AnyRef::Pane(*p), *run),
        Value::App(Fux::Tab(t, run)) => Data::Handle(fux_wire::command::AnyRef::Tab(*t), *run),
        Value::App(Fux::Workspace(w, run)) => {
            Data::Handle(fux_wire::command::AnyRef::Workspace(*w), *run)
        }
        Value::App(Fux::Client(c, run)) => Data::Client(*c, *run),
        Value::App(Fux::Opts(_) | Fux::Extent(_) | Fux::Answer(..)) => {
            return Err(fail());
        }
    })
}

pub(super) fn value(v: &Data, b: &mut Budget) -> Result<Value<Fux>, Failure> {
    b.slots::<Value<Fux>>(1)?;
    Ok(match v {
        Data::Int(v) => Value::Int(*v),
        Data::Bool(v) => Value::Bool(*v),
        Data::Text(v) => {
            if v.chars().count() > fuxth::LONGEST {
                return Err(fail());
            }
            record::imported(b, v)?;
            Value::str(b.string(v)?)
        }
        Data::List(v) => {
            let mut out = b.vector(v.len())?;
            for v in v {
                out.push(value(v, b)?);
            }
            b.charge(fuxth::Shared::<fuxth::List<Fux>>::bytes(0))?;
            Value::List(fuxth::Shared::new(fuxth::List::new(out).ok_or_else(fail)?))
        }
        Data::Record {
            of,
            fields: entries,
        } => {
            let mut fields = b.vector(entries.len())?;
            for (key, v) in entries {
                fields.push((b.string(key)?, value(v, b)?));
            }
            record::from_fields(fields, *of, b)?
        }
        Data::All => Value::App(Fux::All),
        Data::Null => Value::App(Fux::Null),
        Data::Handle(id, run) => Value::App(super::handle(*id, *run)),
        Data::Client(c, run) => Value::App(Fux::Client(*c, *run)),
    })
}

/// The receiving dictionary checks actual arguments before publishing a screen.
pub fn admit(
    mut open: Open,
    state: Option<&crate::client_state::Draft>,
    context: &fux_wire::protocol::Context,
) -> Result<crate::screens::Screen, super::Failed> {
    use fux_wire::wire::Wire;
    let refused = |why| super::Failed::new(fux_wire::code::Code::BadValue, why);
    let past = |why| super::Failed::new(fux_wire::code::Raise::TooLong, why);
    let most = fux_wire::callback::JOB_BYTES;
    if open.bytes() > most {
        return Err(past(format!(
            "screen retained data is past a job's {most} bytes"
        )));
    }
    let mut encoded =
        fux_wire::wire::Writer::counting(fux_wire::protocol::MAX_FRAME.saturating_sub(128));
    open.put(&mut encoded);
    encoded
        .check()
        .map_err(|_| past("screen data is past a frame".to_owned()))?;
    let config = state.and_then(|s| s.config.as_ref());
    let check =
        |action: &fux_wire::callback::Callback, args: Vec<Data>| -> Result<(), super::Failed> {
            crate::screens::admit_source(&action.code)?;
            let job = crate::screens::Job::call(action, args);
            if job.bytes() > most {
                return Err(past(format!(
                    "callback arguments are past a job's {most} bytes"
                )));
            }
            let mut of = context.of;
            for v in job.given.iter().flatten() {
                runs(v, &mut of).map_err(super::callback_failed)?;
            }
            job.machine(config).map(drop).map_err(|e| {
                super::Failed::new(
                    fux_wire::code::Code::BadValue,
                    format!("{}, in the callback", e.located()),
                )
            })
        };
    match &mut open {
        Open::List(l) => {
            let Some(client) = context.client else {
                return Err(refused("a list needs its receiving client"));
            };
            l.client = Data::Client(client, context.of);
            crate::screens::admit_source(&l.action.code)?;
            let tokens = fuxth::token::source("callback", &l.action.code)?;
            super::callback_source(config, &tokens)?;
            let mut of = context.of;
            for arg in &l.action.args {
                runs(arg, &mut of).map_err(super::callback_failed)?;
            }

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
            let mut kinds = std::collections::HashSet::new();
            let mut budget = Budget::with_limit(fux_wire::callback::JOB_BYTES);
            for item in &mut l.items {
                let mut of = context.of;
                runs(&item.payload, &mut of).map_err(super::callback_failed)?;
                let v = value(&item.payload, &mut budget).map_err(super::callback_failed)?;
                if kinds.insert(v.kind_name()) {
                    let args = vec![l.client.clone(), item.payload.clone()];
                    check(&l.action, args)?;
                }
                item.label = fux_wire::words::clean(&item.label);
            }
            l.title = fux_wire::words::clean(&l.title);
        }
        Open::Prompt(p) => {
            if let Some(action) = &p.action {
                check(action, vec![Data::Text(String::new())])?;
            }
            p.title = fux_wire::words::clean(&p.title);
        }
        Open::Copy => {}
    }
    Ok(open.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestResult;
    use crate::screens::{Job, Screen};
    use crate::script::{Asks, Failed, Host};
    use fux_wire::callback::{Callback, Item, ListScreen};
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
            action: code.into(),
            // Receiving admission supplies its actual client.
            client: Data::Null,
        })
    }
    fn run(job: &crate::screens::Job) -> Result<String, Failed> {
        let mut machine = job.machine(None)?;
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        crate::script::run_capped(
            &mut machine,
            &mut host,
            &mut NoRequests,
            Some(100_000),
            None,
        )?;
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
        state.config = Some(std::sync::Arc::new(crate::script::source::text(
            "receiver",
            "define receiver-only nip str value get json print end",
        )?));
        let spec = list(payload, "receiver-only");
        use fux_wire::protocol::{Decoder, Frame, ToAttached};
        let encoded = ToAttached::Open(spec).encode()?;
        let mut decoder = Decoder::default();
        decoder.push(&encoded);
        let Some(ToAttached::Open(spec)) = decoder.frame::<ToAttached>()? else {
            return Err("typed screen relay".into());
        };
        assert!(admit(spec.clone(), None, &context).is_err());
        let Screen::List(screen) = admit(spec, Some(&state), &context)? else {
            return Err("list".into());
        };
        assert!(
            !screen
                .items
                .iter()
                .any(|i| i.label.contains(char::is_control))
        );
        let item = screen.items.first().ok_or("row")?;
        let value = value(&item.payload, &mut Budget::new()).map_err(|e| e.message)?;
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
        let mut machine = job.machine(state.config.as_ref())?;
        let mut out = Vec::new();
        let mut host = Host::new(context, None, &mut out);
        crate::script::run_capped(
            &mut machine,
            &mut host,
            &mut NoRequests,
            Some(100_000),
            None,
        )?;
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
        let mut values =
            crate::script::answered(reply, &fuxth::Imports::new(fux_wire::inspect::BYTES))?;
        let root = values.pop().ok_or("snapshot")?;
        let grant = fuxth::Imports::new(fux_wire::inspect::BYTES);
        root.retain(&grant).map_err(|e| e.message)?;
        let payload = data(&root, &mut Budget::new()).map_err(|e| e.message)?;
        let bytes = fux_wire::wire::encode(&payload)?;
        let payload: Data = fux_wire::wire::Reader::new(&bytes).whole()?;
        let restored = value(&payload, &mut Budget::new()).map_err(|e| e.message)?;
        assert_eq!(
            data(&restored, &mut Budget::new()).map_err(|e| e.message)?,
            payload
        );
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
        child.retain(&grant).map_err(|e| e.message)?;
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
        )?
        else {
            return Err("list".into());
        };
        let payload = screen.items.first().ok_or("row")?.payload.clone();
        assert_eq!(
            run(&Job::call(&screen.action, [screen.client, payload]))?,
            "all\n"
        );
        let child = data(&child, &mut Budget::new()).map_err(|e| e.message)?;
        let text = run(&Job::call(
            &Callback::from("str id get kind print"),
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
        let original = data(&record, &mut Budget::new()).map_err(|e| e.message)?;
        let bytes = fux_wire::wire::encode(&original)?;
        let decoded = fux_wire::wire::Reader::new(&bytes).whole()?;
        let restored = value(&decoded, &mut Budget::new()).map_err(|e| e.message)?;
        let payload = data(&restored, &mut Budget::new()).map_err(|e| e.message)?;
        assert_eq!(payload, original);
        assert!(admit(list(payload.clone(), "2drop"), None, &context).is_ok());
        let other = "3-4".parse().map_err(|()| "run")?;
        let stale = Context {
            of: Some(other),
            ..context.clone()
        };
        assert!(admit(list(payload.clone(), "2drop"), None, &stale).is_err());
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
        assert!(admit(list(nested.clone(), "2drop"), None, &context).is_ok());
        assert!(admit(list(nested.clone(), "2drop"), None, &stale).is_err());
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
                }
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn callbacks_check_actual_kinds_and_empty_results_before_and_after_execution() -> TestResult {
        let context = context()?;
        for (payload, code) in [
            (Data::Text("host".into()), "1 roll drop 1 add drop"),
            (Data::Int(1), "2drop 7"),
        ] {
            assert!(
                admit(list(payload, code), None, &context).is_err(),
                "{code}"
            );
        }
        // The checker deliberately cannot know a list's elements or loop count.
        let callback = Callback::from("nip first 1 add drop");
        let job = Job::call(
            &callback,
            [Data::Null, Data::List(vec![Data::Text("inert".into())])],
        );
        assert!(job.machine(None).is_ok());
        assert!(run(&job).is_err());
        let job = Job::call(
            &Callback::from("2drop list 7 end each end"),
            [Data::Null, Data::Int(1)],
        );
        assert!(job.machine(None).is_ok());
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
        assert!(admit(spec, None, &context).is_err());
        let huge = list(
            Data::Text(std::iter::repeat_n('x', fux_wire::protocol::MAX_FRAME).collect()),
            "2drop",
        );
        assert!(admit(huge, None, &context).is_err());
        let huge = list(
            Data::Int(0),
            &std::iter::repeat_n(' ', crate::screens::ACTION_MAX + 1).collect::<String>(),
        );
        assert!(admit(huge, None, &context).is_err());
        let mut budget = Budget::new();
        assert!(
            data(
                &Value::App(Fux::Opts(std::rc::Rc::new(Default::default()))),
                &mut budget
            )
            .is_err()
        );
        Ok(())
    }
}
