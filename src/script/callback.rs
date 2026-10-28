//! One bounded bridge between language values and owned screen arguments.
use super::{Fux, Value, record};
use crate::callback::Data;
use crate::wire::Budget;
use fuxth::Failure;
use std::rc::Rc;

fn runs(v: &Data, of: &mut Option<crate::command::Run>) -> Result<(), Failure> {
    match v {
        Data::Handle(_, r) | Data::Client(_, r) => {
            *of = super::same_run(*of, *r)?;
        }
        Data::List(v) => {
            for v in v {
                runs(v, of)?;
            }
        }
        Data::Record(v) => {
            for (_, v) in v {
                runs(v, of)?;
            }
        }
        Data::Snapshot(s) => {
            *of = super::same_run(*of, Some(s.run))?;
        }
        Data::Int(_) | Data::Bool(_) | Data::Text(_) | Data::Null => {}
    }
    Ok(())
}

fn fail() -> Failure {
    Failure::new(
        crate::code::Code::BadValue.into(),
        "a callback argument must be an immutable value: scalar, handle, record, null or list",
    )
}
fn charged<T>(result: Result<T, crate::session::Error>) -> Result<T, Failure> {
    result.map_err(super::words::observation_failure)
}

pub(super) fn data(v: &Value<Fux>, b: &mut Budget) -> Result<Data, Failure> {
    if v.depth() > fuxth::NESTING || v.weight() > fuxth::TOTAL {
        return Err(fail());
    }
    charged(b.slots::<Data>(1))?;
    Ok(match v {
        Value::Int(v) => Data::Int(*v),
        Value::Bool(v) => Data::Bool(*v),
        Value::Str(v) => Data::Text(charged(b.string(v.as_ref()))?),
        Value::List(v) => {
            let mut out = charged(b.vector(v.items().len()))?;
            for v in v.items() {
                out.push(data(v, b)?);
            }
            Data::List(out)
        }
        Value::App(Fux::Record(r)) => {
            if let Some(snapshot) = &r.snapshot {
                charged(b.charge(snapshot.heap_bytes()?))?;
                return Ok(Data::Snapshot(Box::new(snapshot.clone())));
            }
            let mut fields = charged(b.vector(r.fields.len()))?;
            for (key, v) in &r.fields {
                fields.push((charged(b.string(key))?, data(v, b)?));
            }
            Data::Record(fields)
        }
        Value::App(Fux::Null) => Data::Null,
        Value::App(Fux::Pane(p, run)) => Data::Handle(crate::command::AnyRef::Pane(*p), *run),
        Value::App(Fux::Tab(t, run)) => Data::Handle(crate::command::AnyRef::Tab(*t), *run),
        Value::App(Fux::Workspace(w, run)) => {
            Data::Handle(crate::command::AnyRef::Workspace(*w), *run)
        }
        Value::App(Fux::Client(c, run)) => Data::Client(*c, *run),
        Value::App(
            Fux::Rendered(_) | Fux::All(_) | Fux::Opts(_) | Fux::Extent(_) | Fux::Answer(..),
        ) => return Err(fail()),
    })
}

pub(super) fn value(v: &Data, b: &mut Budget) -> Result<Value<Fux>, Failure> {
    charged(b.slots::<Value<Fux>>(1))?;
    Ok(match v {
        Data::Int(v) => Value::Int(*v),
        Data::Bool(v) => Value::Bool(*v),
        Data::Text(v) => {
            if v.chars().count() > fuxth::LONGEST {
                return Err(fail());
            }
            charged(b.text_value(v))?;
            Value::str(charged(b.string(v))?)
        }
        Data::List(v) => {
            let mut out = charged(b.vector(v.len()))?;
            for v in v {
                out.push(value(v, b)?);
            }
            charged(b.slots::<fuxth::List<Fux>>(1))?;
            charged(b.slots::<usize>(2))?;
            Value::List(Rc::new(fuxth::List::new(out).ok_or_else(fail)?))
        }
        Data::Record(v) => {
            let mut fields = charged(b.vector(v.len()))?;
            for (key, v) in v {
                fields.push((charged(b.string(key))?, value(v, b)?));
            }
            record::from_fields(fields, b)?
        }
        Data::Snapshot(s) => {
            charged(b.charge(s.heap_bytes()?))?;
            record::snapshot_in((**s).clone(), b)?
        }
        Data::Null => Value::App(Fux::Null),
        Data::Handle(id, run) => Value::App(super::handle(*id, *run)),
        Data::Client(c, run) => Value::App(Fux::Client(*c, *run)),
    })
}

impl crate::screens::Open {
    /// Aggregate retained storage, including callback code and row arguments.
    pub fn bytes(&self) -> usize {
        use crate::screens::Open;
        let callback = |c: &crate::callback::Callback| {
            c.args
                .iter()
                .fold(c.code.capacity(), |n, a| n.saturating_add(a.bytes()))
        };
        std::mem::size_of::<Self>().saturating_add(match self {
            Open::List(l) => l.items.iter().fold(
                l.items
                    .capacity()
                    .saturating_mul(std::mem::size_of::<crate::screens::Item>())
                    .saturating_add(
                        l.title
                            .capacity()
                            .saturating_add(callback(&l.action))
                            .saturating_add(l.client.bytes()),
                    ),
                |n, i| {
                    n.saturating_add(i.label.capacity())
                        .saturating_add(i.payload.bytes())
                },
            ),
            Open::Prompt(p) => p
                .title
                .capacity()
                .saturating_add(p.line.before().len().saturating_add(p.line.after().len()))
                .saturating_add(p.action.as_ref().map_or(0, callback)),
            Open::Confirm(c) => c.question.capacity().saturating_add(callback(&c.action)),
            Open::Copy => 0,
        })
    }

    /// The receiving dictionary checks actual arguments before publishing a screen.
    pub fn admit(
        mut self,
        state: Option<&crate::client_state::Draft>,
        context: &crate::protocol::Context,
    ) -> Result<crate::screens::Screen, super::Failed> {
        use crate::screens::Open;
        use crate::wire::Wire;
        let refused = |why| super::Failed::new(crate::code::Code::BadValue, why);
        if self.bytes() > crate::client::JOB_BYTES {
            return Err(refused("screen retained data exceeds the job byte limit"));
        }
        let mut encoded =
            crate::wire::Writer::counting(crate::protocol::MAX_FRAME.saturating_sub(128));
        self.put(&mut encoded);
        encoded
            .check()
            .map_err(|_| refused("screen data exceeds a frame"))?;
        let config = state.and_then(|s| s.config.as_ref());
        let check =
            |action: &crate::callback::Callback, args: Vec<Data>| -> Result<(), super::Failed> {
                if action.code.len() > crate::screens::ACTION_MAX {
                    return Err(refused("callback source exceeds its byte limit"));
                }
                let job = action.job(args);
                if job.bytes() > crate::client::JOB_BYTES {
                    return Err(refused("callback arguments exceed the job byte limit"));
                }
                let mut of = context.of;
                for v in job.given.iter().flatten() {
                    runs(v, &mut of).map_err(super::callback_failed)?;
                }
                job.machine(config).map(drop).map_err(|e| {
                    super::Failed::new(
                        crate::code::Code::BadValue,
                        format!("{}, in the callback", e.located()),
                    )
                })
            };
        match &mut self {
            Open::List(l) => {
                let Some(client) = context.client else {
                    return Err(refused("a list needs its receiving client"));
                };
                l.client = Data::Client(client, context.of);
                if l.action.code.len() > crate::screens::ACTION_MAX {
                    return Err(refused("callback source exceeds its byte limit"));
                }
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
                let mut budget = Budget::with_limit(crate::client::JOB_BYTES);
                for item in &mut l.items {
                    let mut of = context.of;
                    runs(&item.payload, &mut of).map_err(super::callback_failed)?;
                    let v = value(&item.payload, &mut budget).map_err(super::callback_failed)?;
                    if kinds.insert(v.kind_name()) {
                        let args = vec![l.client.clone(), item.payload.clone()];
                        check(&l.action, args)?;
                    }
                    item.label = crate::words::clean(&item.label);
                }
                l.title = crate::words::clean(&l.title);
            }
            Open::Prompt(p) => {
                if let Some(action) = &p.action {
                    check(action, vec![Data::Text(String::new())])?;
                }
                p.title = crate::words::clean(&p.title);
            }
            Open::Confirm(c) => {
                check(&c.action, Vec::new())?;
                c.question = crate::words::clean(&c.question);
            }
            Open::Copy => {}
        }
        Ok(self.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callback::Callback;
    use crate::protocol::{Asked, Context, Reply};
    use crate::screens::{Item, ListScreen, Open, Screen};
    use crate::script::{Asks, Failed, Host};

    struct NoRequests;
    impl Asks for NoRequests {
        fn ask(&mut self, _: &Asked, _: &mut dyn std::io::Write) -> Result<Reply, Failed> {
            Err(Failed::new(
                crate::code::Code::Client,
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
    fn callbacks_use_the_receiving_dictionary_and_preserve_nested_typed_data() -> Result<(), String>
    {
        let context = context()?;
        let handle = Data::Handle("%3".parse().map_err(|()| "pane")?, context.of);
        let payload = Data::Record(vec![(
            "value".into(),
            Data::List(vec![
                handle,
                Data::Null,
                Data::Bool(true),
                Data::Int(-3),
                Data::Text("' drop %1 kill".into()),
            ]),
        )]);
        let mut state = crate::script::default_state()
            .map_err(|e| e.to_string())?
            .draft();
        state.config = Some(std::sync::Arc::new(
            crate::script::source::text(
                "receiver",
                "define receiver-only nip str value get json print end",
            )
            .map_err(|e| e.to_string())?,
        ));
        let spec = list(payload, "receiver-only");
        use crate::protocol::{Decoder, Frame, ToAttached};
        let encoded = ToAttached::Open(spec).encode().map_err(|e| e.to_string())?;
        let mut decoder = Decoder::default();
        decoder.push(&encoded);
        let Some(ToAttached::Open(spec)) =
            decoder.frame::<ToAttached>().map_err(|e| e.to_string())?
        else {
            return Err("typed screen relay".into());
        };
        assert!(spec.clone().admit(None, &context).is_err());
        let Screen::List(screen) = spec
            .admit(Some(&state), &context)
            .map_err(|e| e.to_string())?
        else {
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
            Some(&Value::App(Fux::Pane(crate::id::PaneId::of(3), context.of)))
        );
        assert_eq!(values.items().get(1), Some(&Value::App(Fux::Null)));
        let job = screen.action.job([screen.client, item.payload.clone()]);
        let mut machine = job
            .machine(state.config.as_ref())
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut host = Host::new(context, None, &mut out);
        crate::script::run_capped(
            &mut machine,
            &mut host,
            &mut NoRequests,
            Some(100_000),
            None,
        )
        .map_err(|e| e.to_string())?;
        assert!(String::from_utf8_lossy(&out).contains("[\"%3\",null,true,-3,\"' drop %1 kill\"]"));
        Ok(())
    }

    #[test]
    fn callback_records_keep_their_held_snapshot_presentation() -> Result<(), String> {
        let (mut session, _) = crate::session::testing::attached(8, 30)?;
        let reply = crate::session::InProcess(&mut session)
            .ask(
                &Asked {
                    after: None,
                    context: Context::default(),
                    request: crate::command::Command::Inspect {
                        scope: crate::inspect::Scope::All,
                        projection: crate::inspect::Projection::Topology,
                    }
                    .into(),
                },
                &mut Vec::new(),
            )
            .map_err(|e| e.to_string())?;
        let values = crate::script::answered(reply).map_err(|e| e.to_string())?;
        let record = values.first().ok_or("snapshot")?;
        let data = data(record, &mut Budget::new()).map_err(|e| e.message)?;
        let text = run(&Callback::from("print").job([data])).map_err(|e| e.to_string())?;
        assert!(text.starts_with("+1\n"), "{text}");
        assert!(text.contains("client c1"), "{text}");
        Ok(())
    }

    #[test]
    fn callbacks_check_actual_kinds_and_empty_results_before_and_after_execution()
    -> Result<(), String> {
        let context = context()?;
        for (payload, code) in [
            (Data::Text("host".into()), "1 roll drop 1 add drop"),
            (Data::Int(1), "2drop 7"),
        ] {
            assert!(list(payload, code).admit(None, &context).is_err(), "{code}");
        }
        // The checker deliberately cannot know a list's elements or loop count.
        let callback = Callback::from("nip first 1 add drop");
        let job = callback.job([Data::Null, Data::List(vec![Data::Text("inert".into())])]);
        assert!(job.machine(None).is_ok());
        assert!(run(&job).is_err());
        let job = Callback::from("2drop list 7 end each end").job([Data::Null, Data::Int(1)]);
        assert!(job.machine(None).is_ok());
        assert!(run(&job).is_err_and(|e| e.message.contains("empty stack")));
        Ok(())
    }

    #[test]
    fn screen_admission_checks_every_payload_run_and_aggregate_bound() -> Result<(), String> {
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
        assert!(spec.admit(None, &context).is_err());
        let huge = list(
            Data::Text(std::iter::repeat_n('x', crate::protocol::MAX_FRAME).collect()),
            "2drop",
        );
        assert!(huge.admit(None, &context).is_err());
        let huge = list(
            Data::Int(0),
            &std::iter::repeat_n(' ', crate::screens::ACTION_MAX + 1).collect::<String>(),
        );
        assert!(huge.admit(None, &context).is_err());
        let mut budget = Budget::new();
        assert!(
            data(
                &Value::App(Fux::Opts(Rc::new(Default::default()))),
                &mut budget
            )
            .is_err()
        );
        Ok(())
    }
}
