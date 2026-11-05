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
    within(v, fux_wire::callback::DEPTH)
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
        _ => {
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
        _ => {}
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
}
