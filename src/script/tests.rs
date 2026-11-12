use super::*;
use crate::TestResult;
struct NoRequests;
impl Asks for NoRequests {
    fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
        testing::found(asked).ok_or_else(|| Failed::new(Code::Client, "unexpected request"))
    }
}
#[test]
fn one_argument_is_a_program() -> TestResult {
    let mut out = Vec::new();
    assert_eq!(main(&["(add 1 2)".into()], None, &mut out)?, 0);
    assert_eq!(out, b"3\n");
    out.clear();
    assert_eq!(
        main(
            &["(first (arguments))".into(), "(data \"quoted\")".into()],
            None,
            &mut out
        )?,
        0
    );
    assert_eq!(out, b"(data \"quoted\")\n");
    Ok(())
}
#[test]
fn release_flags_bypass_the_operand_taking_version_word() -> TestResult {
    for flag in ["--version", "-V"] {
        let mut out = Vec::new();
        assert_eq!(main(&[flag.into()], None, &mut out)?, 0);
        assert_eq!(
            String::from_utf8(out)?,
            format!("{}\n", fux_wire::protocol::Build::OURS)
        );
    }
    Ok(())
}
#[test]
fn a_list_left_prints_an_item_a_line() -> TestResult {
    let mut out = Vec::new();
    main(&["(list 1 \"\" (list 2 \"x\") 3)".into()], None, &mut out)?;
    assert_eq!(out, b"1\n\n(2 \"x\")\n3\n");
    out.clear();
    main(&["()".into()], None, &mut out)?;
    assert!(out.is_empty());
    Ok(())
}
#[test]
fn a_caught_closed_pipe_still_ends_quietly() -> TestResult {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        main(
            &["(catch-kind! (print \"x\") output failure #inert)".into()],
            None,
            &mut Closed
        )?,
        141
    );
    Ok(())
}
#[test]
fn final_output_reports_the_writers_failure() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("broken writer"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let failure = main(&["1".into()], None, &mut Broken).err();
    assert!(failure.is_some_and(|f| f.code == Code::Output && f.message.contains("broken writer")));
}
#[test]
fn final_output_streams_a_large_string_without_copying_it() -> TestResult {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let text = Value::<Fux>::str(std::iter::repeat_n('x', 2 << 20).collect::<String>());
    let mut sink = Count(0);
    let allocator = fux_vt_peak::ThreadPeak;
    let before = allocator.live();
    allocator.reset_peak();
    {
        let mut out = io::BufWriter::with_capacity(64 << 10, &mut sink);
        writeln!(out, "{}", value::Text(&text))?;
        out.flush()?;
    }
    assert_eq!(sink.0, (2 << 20) + 1);
    assert!(allocator.peak().saturating_sub(before) <= 64 << 10);
    Ok(())
}
#[test]
fn version_text_from_another_run_is_read_without_asking() -> TestResult {
    // A key's program runs on its server's run; the version is another's.
    let context = Context {
        of: Some("2-0".parse().map_err(|()| "a run")?),
        ..Context::default()
    };
    let mut machine = Machine::source("key", "(version \"%1:0.7.0.0.0:1-1\")");
    let mut out = Vec::new();
    let mut host = Host::new(context, None, &mut out);
    let values = run(&mut machine, &mut host, &mut NoRequests, None)?;
    assert_eq!(values, [Value::str("%1:0.7.0.0.0:1-1")]);
    Ok(())
}
#[test]
fn held_answers_and_captures_project_without_refreshing() -> TestResult {
    let version: Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
    let reply = Reply {
        run: Some(version.run),
        response: Response::Answered(PaneResults::One(PaneAnswer::Acted(version))),
    };
    let values = testing::held_reply(
        "(get (send-keys (text (pane 1) \"x\")) \"version\")",
        reply,
        &mut Vec::new(),
    )?;
    assert_eq!(values, [Value::str(version.to_string())]);
    Ok(())
}
#[test]
fn only_the_command_line_asks_its_process() -> TestResult {
    for source in ["(input)", "(read-file \"/etc/passwd\")", "(exit 7)"] {
        let result = testing::evaluate(source, None, &mut NoRequests, &mut Vec::new());
        assert!(result.is_err_and(|f| f.code == Code::NotHere), "{source}");
    }
    let values = testing::evaluate("(arguments)", None, &mut NoRequests, &mut Vec::new())?;
    assert!(matches!(values.as_slice(),[Value::List(items)] if items.items().is_empty()));
    Ok(())
}
#[test]
fn the_default_keys_are_a_config_list_keys_prints() -> TestResult {
    let defaults = default_state()?;
    let source = defaults.keys.describe();
    let path = std::env::temp_dir().join(format!("default-keys-{}.grift", std::process::id()));
    std::fs::write(&path, &source)?;
    let state = run_file(&path);
    std::fs::remove_file(path)?;
    assert_eq!(state?.keys, defaults.keys);
    // Whatever a label holds, in a binding's or a screen's key.
    let labels =
        "(bind-key \"F9\" (code! (lambda! () #inert))) (label \"\u{26a0}\u{fe0f} zap\u{ad}\")
        (bind-action \"z\" (code! (lambda! (c p) #inert))) (label \"\u{200b}z\")";
    let path = std::env::temp_dir().join(format!("label-keys-{}.grift", std::process::id()));
    std::fs::write(&path, labels)?;
    let labelled = run_file(&path)?;
    std::fs::write(&path, labelled.keys.describe())?;
    let state = run_file(&path);
    std::fs::remove_file(path)?;
    assert_eq!(state?.keys, labelled.keys);
    Ok(())
}
#[test]
fn an_exit_cannot_be_caught_as_an_ordinary_error() -> TestResult {
    let mut out = Vec::new();
    let status = main(
        &["(catch! (exit 7) failure (display \"continued\"))".into()],
        None,
        &mut out,
    )?;
    assert_eq!(status, 7);
    assert!(out.is_empty());
    Ok(())
}
#[test]
fn an_exit_runs_the_cleanups_around_it() -> TestResult {
    let mut out = Vec::new();
    let status = main(
        &["(finally! (exit 3) (display \"cleaned\"))".into()],
        None,
        &mut out,
    )?;
    assert_eq!((status, out.as_slice()), (3, &b"cleaned"[..]));
    Ok(())
}
#[test]
fn explicit_code_runs_in_its_receiving_dictionary() -> TestResult {
    let mut out = Vec::new();
    main(&["(define! receiver (lambda! (x) (add x 1))) (execute! (code! (lambda! (x) (receiver x))) 41)".into()],None,&mut out)?;
    assert_eq!(out, b"42\n");
    Ok(())
}

use super::testing::{evaluate, held_reply};
use fux_wire::json::Json;
fn observed(
    version: super::Version,
    terminal: Vec<(fux_wire::inspect::Terminal, Json)>,
) -> fux_wire::inspect::Pane {
    let launch = fux_wire::inspect::Launch {
        shell: false,
        cwd: fux_wire::inspect::Path::Utf8("/".into()),
    };
    fux_wire::inspect::Pane {
        id: version.pane,
        tab: fux_wire::id::TabId::of(1),
        version,
        name: None,
        label: String::new(),
        title: String::new(),
        rows: 24,
        cols: 80,
        row: None,
        col: None,
        facts: Some(fux_wire::inspect::Facts {
            program: Json::Null,
            launch,
            terminal,
        }),
        cwd: None,
    }
}

fn large_plain(version: super::Version) -> fux_wire::answer::Captured {
    fux_wire::answer::Captured {
        of: fux_wire::answer::Of::Pane {
            pane: Box::new(observed(version, Vec::new())),
            read: Vec::new(),
            wrap_pending: false,
        },
        screen_cursor: None,
        lines: vec![fux_wire::answer::Line {
            text: std::iter::repeat_n('x', 17_000_000).collect(),
            runs: Vec::new(),
        }],
        styles: Vec::new(),
        styled: false,
        marks: false,
        changed_rows: None,
    }
}

#[test]
fn large_plain_capture_crosses_the_machine_boundary() -> TestResult {
    let version: super::Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
    let capture = large_plain(version);
    let reply = super::Reply {
        run: Some(version.run),
        ..fux_wire::protocol::Body::Capture(Box::new(capture)).into()
    };
    let mut out = Vec::new();
    held_reply(
        "(sequence! (print (capture (all-history (pane 1)))) #inert)",
        reply,
        &mut out,
    )?;
    assert_eq!(out.len(), 17_000_001);
    Ok(())
}

#[test]
fn ended_answers_expose_both_exit_fields() -> TestResult {
    use super::*;
    use fux_wire::answer::Ended;
    let version: Version = "%1:0.7.0.0.0:1-1".parse().map_err(|()| "version")?;
    for (ended, exited, signal) in [
        (Ended::Exited(7), Value::Int(7), Value::App(Fux::Null)),
        (Ended::Signalled(9), Value::App(Fux::Null), Value::Int(9)),
        (Ended::Lost, Value::App(Fux::Null), Value::App(Fux::Null)),
    ] {
        let answer = PaneAnswer::Changed {
            version,
            changed: Vec::new(),
            ended: Some(ended),
        };
        let mut json = String::new();
        answer.write_json(&mut json)?;
        assert!(json.contains("\"exited\":"), "{json}");
        assert!(json.contains("\"signal\":"), "{json}");
        let reply = Reply {
            run: Some(version.run),
            response: Response::Answered(PaneResults::One(answer)),
        };
        let values = held_reply(
            "(sequence! (define! variables-6 (context ())) (bind variables-6 (quote! held) (wait (timeout (pane 1) 0))) (define! value-10 (evaluate (quote! held) variables-6)) \"exited\" (define! value-12 (get value-10 \"exited\")) (define! value-13 (evaluate (quote! held) variables-6)) \"signal\" (list value-12 (get value-13 \"signal\")))",
            reply,
            &mut Vec::new(),
        )?;
        assert_eq!(
            values,
            vec![Value::List(Rc::new(List::new(vec![exited, signal])))]
        );
    }
    Ok(())
}

#[test]
fn every_observation_wait_encodes_its_timeout_and_a_bare_one_none() -> TestResult {
    use super::*;
    use fux_wire::command::{AnyRef, Command, Request};
    use fux_wire::wire::Reader;
    struct Record(Vec<Option<u32>>);
    impl Asks for Record {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            let bytes = fux_wire::wire::encode(asked);
            let decoded: Asked = Reader::new(&bytes)
                .whole()
                .map_err(|e| Failed::new(Code::Usage, e.to_string()))?;
            if let Request::Wait(wait) = decoded.request {
                self.0.push(wait.timeout.map(Limit::seconds));
                Err(Failed::new(Code::NoServer, "recorded"))
            } else if let Request::Do(fux_wire::command::Command::Resolve { .. }) = decoded.request
            {
                Ok(Body::Found(AnyRef::Pane(
                    "%1".parse()
                        .map_err(|e| Failed::new(Code::Usage, format!("{e:?}")))?,
                ))
                .into())
            } else if let Request::Do(Command::SendKeys { .. }) = decoded.request {
                let version = "%1:0.0.0.0.0:1-2"
                    .parse()
                    .map_err(|()| Failed::new(Code::Usage, "version"))?;
                Ok(PaneResults::One(PaneAnswer::Acted(version)).into())
            } else {
                Err(Failed::new(Code::Usage, "unexpected request"))
            }
        }
    }
    let bundled = default_state()?;
    for subject in [
        "(pane 1)",
        "(changed \"%1:0.0.0.0.0:1-2\")",
        "(send-keys (key (pane 1) \"Enter\"))",
    ] {
        for limit in [None, Some(0), Some(7)] {
            let option = match limit {
                Some(n) => format!("(timeout {subject} {n})"),
                None => subject.to_owned(),
            };
            let mut draft = bundled.draft();
            let mut out = io::sink();
            let mut record = Record(Vec::new());
            let failed = evaluate(
                &format!("(wait {option})"),
                Some(&mut draft),
                &mut record,
                &mut out,
            )
            .err()
            .ok_or("wait was not recorded")?;
            assert_eq!(failed.code, Code::NoServer);
            assert_eq!(record.0, [limit], "{subject} {option}");
        }
    }
    Ok(())
}

#[test]
fn local_json_accepts_observations_from_different_runs() -> TestResult {
    use super::*;
    struct Reports(std::collections::VecDeque<Run>);
    impl Asks for Reports {
        fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
            let run = self
                .0
                .pop_front()
                .ok_or_else(|| Failed::new(Code::Usage, "run"))?;
            let cursor = format!("0:{run}")
                .parse()
                .map_err(|()| Failed::new(Code::Usage, "cursor"))?;
            Ok(Reply {
                run: Some(run),
                ..Body::Snapshot(Box::new(fux_wire::inspect::Snapshot {
                    run,
                    cursor,
                    scope: fux_wire::inspect::Scope::All,
                    projection: fux_wire::inspect::Projection::Facts,
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    panes: Vec::new(),
                    clients: Vec::new(),
                }))
                .into()
            })
        }
    }
    let runs = ["1-0", "2-0"].map(|s| s.parse().map_err(|()| "run"));
    let mut reports = Reports(runs.into_iter().collect::<Result<_, _>>()?);
    let mut sink = Vec::new();
    evaluate(
        "(sequence! (define! variables-30 (context ())) (bind variables-30 (quote! a) (list-session)) (bind variables-30 (quote! b) (list-session)) (print (datum (list (evaluate (quote! a) variables-30) (evaluate (quote! b) variables-30)))) #inert)",
        None,
        &mut reports,
        &mut sink,
    )?;
    let text = String::from_utf8(sink)?;
    assert!(
        text.starts_with('(') && text.contains("(run \"1-0\")") && text.contains("(run \"2-0\")")
    );
    Ok(())
}

#[test]
fn arrange_reconciles_its_pane_leaf_runs() -> TestResult {
    use super::*;
    use fux_wire::command::{Command, Request};
    struct Leaves {
        runs: std::collections::VecDeque<Run>,
        submitted: Vec<Option<Run>>,
    }
    impl Asks for Leaves {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            let mut run = None;
            let body = match asked.request {
                Request::Do(fux_wire::command::Command::Resolve { .. }) => {
                    Body::Found(AnyRef::Tab(TabId::of(1)))
                }
                Request::Do(Command::Members { .. }) => {
                    run = self.runs.pop_front();
                    Body::Handles(vec![AnyRef::Pane(PaneId::of(1))])
                }
                Request::Do(Command::Arrange { .. }) => {
                    self.submitted.push(asked.context.of);
                    Body::Empty
                }
                Request::Do(_)
                | Request::Wait(_)
                | Request::NextEvents { .. }
                | Request::Read { .. } => {
                    return Err(Failed::new(Code::Usage, "unexpected request"));
                }
            };
            Ok(Reply { run, ..body.into() })
        }
    }
    let ours: Run = "1-0".parse().map_err(|()| "run")?;
    let other: Run = "2-0".parse().map_err(|()| "run")?;
    for runs in [[ours, ours], [ours, other]] {
        let mut leaves = Leaves {
            runs: runs.into(),
            submitted: Vec::new(),
        };
        let mut machine = Machine::source(
            "layout",
            "(define! a (panes (tab 1))) (define! b (panes (tab 1))) (arrange (tab 1) \"@1:0:1-0\" (list (list \"horizontal\" 1 1 1 2) (first a) (first b)))",
        );
        let mut sink = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut sink);
        let result = run(&mut machine, &mut host, &mut leaves, None);
        if runs == [ours, ours] {
            result?;
            assert_eq!(leaves.submitted, [Some(ours)]);
        } else {
            assert!(result.is_err_and(|f| f.code == Code::BadValue));
            assert!(leaves.submitted.is_empty());
        }
    }
    Ok(())
}

#[test]
fn what_is_left_names_its_word() -> TestResult {
    for (program, end) in [
        ("(paste (pane 2) \"hi\")", "send-keys"),
        ("(in (options) \"x\")", "new-tab"),
    ] {
        let failed = testing::evaluate(program, None, &mut NoRequests, &mut Vec::new())
            .err()
            .ok_or("unused options accepted")?;
        assert_eq!(failed.code, Code::Unused);
        assert!(failed.message.contains(end), "{failed}");
    }
    Ok(())
}

#[test]
fn default_styled_capture_json_is_printed_whole() -> TestResult {
    let version: Version = "%1:0.0.0.0.0:1-1".parse().map_err(|()| "version")?;
    let capture = fux_wire::answer::Captured::default_styled(version);
    let reply = Reply {
        run: Some(version.run),
        ..Body::Capture(Box::new(capture)).into()
    };
    let bytes = fux_wire::wire::encode(&reply);
    let reply = fux_wire::wire::Reader::new(&bytes).whole::<Reply>()?;
    let mut out = Vec::new();
    held_reply(
        "(print (data-text (capture (styles (all-history (pane 1))))))",
        reply,
        &mut out,
    )?;
    assert!(out.len() > 20 << 20, "{}", out.len());
    assert!(out.starts_with(b"((kind \"capture\") (id %1") && out.ends_with(b")\n"));
    Ok(())
}

#[test]
fn pane_outcomes_determine_the_reply_status() -> Result<(), Box<dyn std::error::Error>> {
    use fux_wire::answer::{PaneAnswer, PaneResults};
    use fux_wire::code::Code;
    use fux_wire::id::PaneId;
    use fux_wire::protocol::{End, Reply};
    let version = "%2:0.0.0.0.0:48211-1867a3f9c2b8e7d6"
        .parse()
        .map_err(|()| "version")?;
    let changed = PaneAnswer::Changed {
        version,
        changed: vec![fux_wire::answer::Change::Screen],
        ended: None,
    };
    let closed = PaneAnswer::Closed(PaneId::of(3));
    let timeout = PaneAnswer::TimedOut {
        since: version,
        limit: std::time::Duration::from_millis(1500),
    };
    for (results, code) in [
        (PaneResults::One(PaneAnswer::Acted(version)), None),
        (PaneResults::One(changed.clone()), None),
        (PaneResults::One(closed.clone()), Some(Code::Closed)),
        (PaneResults::One(timeout.clone()), Some(Code::Timeout)),
        (PaneResults::Many(vec![]), None),
        (
            PaneResults::Many(vec![changed.clone(), closed.clone(), timeout.clone()]),
            Some(Code::Closed),
        ),
        (
            PaneResults::Many(vec![changed, timeout, closed]),
            Some(Code::Timeout),
        ),
    ] {
        let reply = Reply {
            run: Some(version.run),
            ..results.into()
        };
        let bytes = fux_wire::wire::encode(&reply);
        let decoded = fux_wire::wire::Reader::new(&bytes).whole::<Reply>()?;
        assert_eq!(decoded, reply);
        assert_eq!(decoded.end().code(), code);
        assert_eq!(
            decoded.end().status(),
            code.map_or(0, |code| code.status().get())
        );
        let values = crate::script::answered(decoded.clone()).map_err(|f| f.to_string())?;
        let value = values.first().ok_or("answer")?;
        match value {
            // Each answer keeps its run; no answers hold none.
            Value::List(items) => {
                for item in items.items() {
                    assert_eq!(scope(item), Some(version.run));
                }
            }
            value @ Value::Int(_)
            | value @ Value::Float(_)
            | value @ Value::Bool(_)
            | value @ Value::Str(_)
            | value @ Value::Symbol(_)
            | value @ Value::App(_)
            | value @ Value::Bytes(_) => assert_eq!(scope(value), Some(version.run)),
        }
        let mut data = String::new();
        value.write_data(&mut data)?;
        assert_eq!(data.contains("(status 124)"), has_timeout(&decoded));
        if let End::Failed { message, .. } = decoded.end() {
            assert!(!message.is_empty());
            if has_timeout(&decoded) {
                assert!(message.contains("unchanged for 1.5s"), "{message}");
            }
        }
    }
    Ok(())
}
fn has_timeout(reply: &super::Reply) -> bool {
    use fux_wire::code::Code;
    matches!(&reply.response, super::Response::Answered(answers) if answers.all().iter().any(|a| a.failure() == Some(Code::Timeout)))
}

#[test]
fn a_batch_left_that_missed_entries_is_refused() -> TestResult {
    use fux_wire::events::{Batch, Status};
    let next = "1:1-0".parse().map_err(|()| "cursor")?;
    for missed in [2, 0] {
        let batch = Batch {
            first: next,
            next,
            missed,
            status: Status::Timeout,
            events: Vec::new(),
        };
        let mut out = Vec::new();
        let result = held_reply(
            r#"(batch-events (next-events (all) "" 0))"#,
            Body::Events(Box::new(batch.clone())).into(),
            &mut out,
        );
        assert!(out.is_empty());
        // Left at a program's end, a batch prints its events alone, and one
        // that missed entries is refused (D-93).
        let held = held_reply(
            r#"(next-events (all) "" 0)"#,
            Body::Events(Box::new(batch)).into(),
            &mut out,
        )?;
        let [Value::App(Fux::Record(record))] = held.as_slice() else {
            return Err("no batch".into());
        };
        assert!(record.is_batch());
        assert_eq!(record.printed_events().is_err(), missed > 0);
        // One loss, one kind, however the batch came to be refused, and the
        // word says so.
        assert!(
            Word::Act(Act::NextEvents)
                .codes()
                .contains(&Code::JournalLoss)
        );
        assert!(
            record
                .printed_events()
                .err()
                .is_none_or(|failure| failure.code.name() == "journal-loss")
        );
        if missed == 0 {
            assert!(result.is_ok());
        } else {
            assert!(
                result.is_err_and(|failure| failure.kind.as_deref() == Some("journal-loss")
                    && failure.message.contains('2'))
            );
        }
    }
    Ok(())
}
#[test]
fn a_deep_list_is_written_as_json_and_refused_past_the_wire() -> TestResult {
    let source = "(define! nest (lambda! (n value) (if! (same? n 0) value (nest (subtract n 1) (list value))))) (byte-length (data-text (nest 100000 ())))";
    let left = evaluate(source, None, &mut NoRequests, &mut Vec::new())?;
    assert_eq!(left, [Value::Int(200002)]);
    let mut value = Value::<Fux>::Int(0);
    for _ in 0..fux_wire::callback::DEPTH {
        value = Value::List(Rc::new(List::new(vec![value])));
    }
    assert!(callback::data(&value).is_ok());
    value = Value::List(Rc::new(List::new(vec![value])));
    assert!(callback::data(&value).is_err());
    Ok(())
}
#[test]
fn a_screen_past_a_grid_is_size_from_the_word_and_the_server() -> TestResult {
    let most = u16::MAX;
    let word = evaluate(
        &format!("(new-tab (size (in (options) \"new-workspace\") {most} {most}))"),
        None,
        &mut NoRequests,
        &mut Vec::new(),
    )
    .map(drop)
    .err()
    .map(|f| f.code);
    let (mut session, client) = testing::attached(8, 30)?;
    let server = session
        .resize(client, most, most, std::time::Instant::now())
        .map(drop)
        .map_err(Failed::from)
        .err()
        .map(|f| f.code);
    assert_eq!([word, server], [Some(Code::Size); 2]);
    Ok(())
}
#[test]
fn attach_defaults_replace_targets_and_clear_names_locally() -> TestResult {
    use fux_wire::command::{AttachTarget, WsRef};
    let mut draft = default_state()?.draft();
    evaluate(
        r#"(attach-defaults (in (options) "work"))"#,
        Some(&mut draft),
        &mut NoRequests,
        &mut Vec::new(),
    )?;
    assert_eq!(
        draft.attach.as_ref().and_then(|a| a.name.as_deref()),
        Some("main")
    );
    assert_eq!(
        draft.attach.as_ref().and_then(|a| a.target.clone()),
        Some(AttachTarget::Workspace(WsRef::Name("work".into())))
    );
    for program in [
        r#"(attach-defaults (in (options) "other"))"#,
        r#"(attach-defaults (name (options) "fresh"))"#,
        "(attach-defaults (options))",
    ] {
        evaluate(program, Some(&mut draft), &mut NoRequests, &mut Vec::new())?;
    }
    let cleared = draft.clone().finish().map_err(|f| f.to_string())?;
    assert!(cleared.attach.target.is_none() && cleared.attach.name.is_none());
    for program in [
        r#"(attach-defaults (name (in (options) "work") "first"))"#,
        r#"(attach-defaults (in (in (options) "work") "other"))"#,
        "(attach-defaults (size (options) 80 24))",
    ] {
        assert!(evaluate(program, Some(&mut draft), &mut NoRequests, &mut Vec::new()).is_err());
        assert_eq!(draft.clone().finish().map_err(|f| f.to_string())?, cleared);
    }
    Ok(())
}

#[test]
fn the_dictionary_is_the_programs_own_as_records() -> TestResult {
    let program = r#"(define! sq (lambda! (x) (multiply x x)))
        (define! sq2 (lambda! (x) (sq x))) (define! sq (lambda! () 0))
        (define! built-in-names ())
        (define! dictionary-value (dictionary))
        (list (get (word "sq") "name") (get (word "sq") "provenance")
              (get (word "first") "provenance") (get (word "split") "provenance")
              (get (word "add") "provenance")
              (length (filter (lambda! (entry) (same? (get entry "name") "sq")) dictionary-value))
              (length (filter (lambda! (entry) (same? (get entry "name") "sq2")) dictionary-value))
              (length (filter (lambda! (entry) (same? (get entry "name") "first")) dictionary-value))
              (length (filter (lambda! (entry) (same? (get entry "name") "built-in-names")) dictionary-value))
              (get (word "capture") "documentation"))"#;
    let left = evaluate(program, None, &mut NoRequests, &mut Vec::new())?;
    let [Value::List(values)] = left.as_slice() else {
        return Err("dictionary results".into());
    };
    let expected = vec![
        Value::str("sq"),
        Value::str("run"),
        Value::str("core"),
        Value::str("fux"),
        Value::str("core"),
        Value::Int(1),
        Value::Int(1),
        Value::Int(1),
        Value::Int(1),
        Value::str(crate::help::explain("capture").ok_or("capture help")?),
    ];
    assert_eq!(values.items(), expected);
    let changed = evaluate(
        r#"(define! split (lambda! () 0)) (get (word "split") "provenance")"#,
        None,
        &mut NoRequests,
        &mut Vec::new(),
    )?;
    assert_eq!(changed, [Value::str("run")]);
    let failure = evaluate(r#"(word "nosuch")"#, None, &mut NoRequests, &mut Vec::new()).err();
    assert!(failure.is_some_and(|failure| failure.code == Code::UnknownWord));
    Ok(())
}
#[test]
fn the_keys_own_words_are_a_clients_not_the_command_lines() -> TestResult {
    let failure = evaluate("(me)", None, &mut NoRequests, &mut Vec::new()).err();
    assert!(failure.is_some_and(|failure| failure.code == Code::NotHere));
    let mut draft = default_state()?.draft();
    let mut out = Vec::new();
    let mut machine = Machine::source("key", "(me)");
    let mut host = Host::job(
        Context {
            client: Some("c1".parse()?),
            ..Default::default()
        },
        &mut draft,
        &mut out,
    );
    assert_eq!(
        run(&mut machine, &mut host, &mut NoRequests, None)?,
        [Value::App(Fux::Client("c1".parse()?, None))]
    );
    // Helpers are normal grift bindings; client scope is checked when they run.
    for name in [
        "me-focused",
        "confirm-close",
        "rename-prompt",
        "choose-among",
        "choose-move",
        "choose-pane",
    ] {
        let source = format!("(get (word {}) \"provenance\")", quoted(name));
        assert_eq!(
            evaluate(&source, None, &mut NoRequests, &mut Vec::new())?,
            [Value::str("library")]
        );
    }
    Ok(())
}

#[test]
fn attach_words_accept_compiled_action_answers_and_keep_their_run() -> TestResult {
    use super::*;
    use fux_wire::command::{AttachTarget, Command, Request};
    struct Record {
        version: Version,
        sent: usize,
        attached: Option<(Option<Run>, Option<AttachTarget>, Option<fux_vt::Size>)>,
    }
    impl Asks for Record {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            let mut reply: Reply = if matches!(
                asked.request,
                Request::Do(fux_wire::command::Command::Resolve { .. })
            ) {
                Body::Found(AnyRef::Pane(self.version.pane)).into()
            } else if matches!(asked.request, Request::Do(Command::SendKeys { .. })) {
                self.sent = self.sent.saturating_add(1);
                PaneResults::One(PaneAnswer::Acted(self.version)).into()
            } else {
                return Err(Failed::new(Code::Usage, "unexpected request"));
            };
            reply.run = Some(self.version.run);
            Ok(reply)
        }
        fn local(
            &mut self,
            what: &Local,
            _: Option<&mut Draft>,
            _: &mut dyn Write,
        ) -> Result<Vec<Value<Fux>>, Failed> {
            let Local::Attach {
                of, target, size, ..
            } = what
            else {
                return Err(Failed::new(Code::Usage, "unexpected local action"));
            };
            self.attached = Some((*of, target.clone(), *size));
            Ok(Vec::new())
        }
    }
    let version: Version = "%1:0.0.0.0.0:1-2".parse().map_err(|()| "version")?;
    for (action, sized) in [
        ("attach-defaults", false),
        ("attach", false),
        ("attach", true),
    ] {
        let subject = "(send-keys (text (pane 1) \"hello\"))";
        let subject = if sized {
            format!("(size {subject} 80 24)")
        } else {
            subject.to_owned()
        };
        let code = format!("({action} {subject})");
        let mut machine = Machine::source("answer-attach", &code);
        let mut draft = default_state()?.draft();
        let mut sink = io::sink();
        let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
        let mut record = Record {
            version,
            sent: 0,
            attached: None,
        };
        run(&mut machine, &mut host, &mut record, None)?;
        assert_eq!(record.sent, 1);
        let target = Some(AttachTarget::Pane(version.pane));
        if action == "attach-defaults" {
            let defaults = draft.attach.ok_or("attach defaults")?;
            assert_eq!((defaults.of, defaults.target), (Some(version.run), target));
        } else {
            let size = sized.then(|| fux_vt::Size::new(24, 80)).transpose()?;
            assert_eq!(record.attached, Some((Some(version.run), target, size)));
        }
    }
    // A baseline is no target: the word refuses it before it attaches.
    for baseline in ["changed", "expect"] {
        for action in ["attach", "attach-defaults"] {
            let code = format!("({action} ({baseline} {}))", quoted(&version.to_string()));
            let mut machine = Machine::source("baseline", &code);
            let mut draft = default_state()?.draft();
            let defaults = draft.attach.clone();
            let mut sink = io::sink();
            let mut host = Host::new(Context::default(), Some(&mut draft), &mut sink);
            let mut record = Record {
                version,
                sent: 0,
                attached: None,
            };
            let ran = run(&mut machine, &mut host, &mut record, None);
            assert!(ran.is_err(), "{code}");
            assert_eq!(record.attached, None, "{code}");
            assert_eq!(draft.attach, defaults, "{code}");
        }
    }
    Ok(())
}

#[test]
fn a_file_that_does_not_read_is_placed_at_its_line() -> TestResult {
    let mut g: grift::Grift = grift::Grift::new().map_err(|e| format!("{e:?}"))?;
    let unread = g
        .evaluate_in(&mut (), &g.top_level(), "keys.grift", "(print 1)\n(first")
        .err()
        .ok_or("it read")?;
    let failure = bridge::failed(&g, unread);
    assert_eq!(failure.place.as_deref(), Some("keys.grift:2"));
    Ok(())
}

#[test]
fn telling_an_uncaught_failure_allocates_no_more_at_depth() -> TestResult {
    let allocator = fux_vt_peak::ThreadPeak;
    let mut told = Vec::new();
    for depth in [10, 5000] {
        let mut g: grift::Grift = grift::Grift::new().map_err(|e| format!("{e:?}"))?;
        let source = format!(
            "(define! deep (lambda! (n) (if! (same? n 0) (first ()) (add (deep (subtract n 1)) 1)))) (deep {depth})"
        );
        let error = g
            .evaluate_in(&mut (), &g.top_level(), "deep", &source)
            .err()
            .ok_or("no failure")?;
        let base = allocator.live();
        allocator.reset_peak();
        let failure = bridge::failed(&g, error);
        let place = failure.trace.first().cloned().unwrap_or_default();
        told.push((allocator.peak().saturating_sub(base), place));
    }
    let [(shallow, said), (deep, again)] = told.as_slice() else {
        return Err("two depths".into());
    };
    // An error names its values, not who failed; the trace says where.
    assert!(said.contains("(first ())") && again.contains("(first ())"));
    assert_eq!(
        shallow, deep,
        "formatting a deep trace should have bounded temporary storage"
    );
    Ok(())
}
#[test]
fn layout_scalar_refusals_do_not_copy_retained_strings() -> TestResult {
    struct Scalars {
        bad: Value<Fux>,
        base: usize,
        peak: usize,
    }
    impl Asks for Scalars {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            testing::found(asked)
                .ok_or_else(|| Failed::new(Code::Client, "invalid scalars submitted a request"))
        }
        fn local(
            &mut self,
            what: &Local,
            _: Option<&mut Draft>,
            _: &mut dyn Write,
        ) -> Result<Vec<Value<Fux>>, Failed> {
            match what {
                Local::Shell(Shell::Input) => Ok(vec![self.bad.clone()]),
                Local::Shell(Shell::Getenv(name)) if name.0 == "MEASURE" => {
                    let allocator = fux_vt_peak::ThreadPeak;
                    self.base = allocator.live();
                    allocator.reset_peak();
                    Ok(vec![Value::str("")])
                }
                Local::Shell(Shell::Getenv(name)) if name.0 == "PEAK" => {
                    self.peak = fux_vt_peak::ThreadPeak.peak().saturating_sub(self.base);
                    Ok(vec![Value::str("")])
                }
                Local::Attach { .. }
                | Local::Start
                | Local::Status
                | Local::Open(_)
                | Local::Shell(_) => Err(Failed::new(Code::Client, "unexpected local operation")),
            }
        }
    }
    let mut peaks = Vec::new();
    for proposal in [
        "(arrange t bad ())",
        r#"(arrange t "@1:0:1-0" (list (list bad 1 1 0 0)))"#,
    ] {
        let bad = Value::str(std::iter::repeat_n('x', 500000).collect::<String>());
        let mut scalars = Scalars {
            bad,
            base: 0,
            peak: 0,
        };
        let source = format!(
            r#"(define! bad (input)) (define! t (tab 1)) (environment-variable "MEASURE") (for-each (lambda! (#ignore) (catch-kind! {proposal} bad-value failure #inert)) (range 0 16)) (environment-variable "PEAK")"#
        );
        evaluate(&source, None, &mut scalars, &mut Vec::new())?;
        peaks.push(scalars.peak);
    }
    assert!(
        peaks.iter().all(|peak| *peak < 64 << 10),
        "scalar refusal peaks: {peaks:?}"
    );
    Ok(())
}

/// Every name a library defines has the word that loads it, and none is
/// left over: `LIBRARY_WORDS` is the libraries' own definitions.
#[test]
fn the_library_words_are_what_the_libraries_define() {
    let mut defined: Vec<&str> = [LIBRARY, include_str!("sessions.grift")]
        .iter()
        .flat_map(|text| text.lines())
        .filter_map(|line| line.strip_prefix("(define! "))
        .filter_map(|rest| rest.split([' ', '(']).next())
        .collect();
    let mut words = bridge::LIBRARY_WORDS.to_vec();
    defined.sort_unstable();
    words.sort_unstable();
    assert_eq!(defined, words);
}

/// A binding's failure names the config line it was written on, and the
/// name that wasn't there.
#[test]
fn a_callback_fails_at_the_line_it_was_written() -> TestResult {
    let source = "\n\n(code! (lambda! ()\n  (nosuch)))";
    let values = evaluate(source, None, &mut NoRequests, &mut Vec::new())?;
    let [Value::App(Fux::Code(code))] = values.as_slice() else {
        return Err("no code value".into());
    };
    let place = code.place.as_ref().ok_or("no place")?;
    assert_eq!((place.file.as_str(), place.line), ("test", 3));
    let program = crate::screens::Program::Code((**code).clone());
    let mut machine = callback_machine(None, &program, Some(&[]), None)?;
    let mut out = Vec::new();
    let mut host = Host::new(Context::default(), None, &mut out);
    let failed = run(&mut machine, &mut host, &mut NoRequests, None)
        .err()
        .ok_or("the callback ran")?;
    assert!(failed.message.contains("nosuch"), "{failed}");
    assert!(failed.to_string().contains("test:4"), "{failed}");
    Ok(())
}

/// What a program prints reads back as the same values: a handle keeps its
/// server run through `read-form`, and a stale one is still refused.
#[test]
fn printed_data_reads_back_with_its_run() -> TestResult {
    let run: Run = "3-4".parse().map_err(|()| "run")?;
    let source = format!(
        "(define! held (quote! %3:{run}))
         (define! text (data-text (list held (list (quote! id) held))))
         (define! again (first (read-form text 0)))
         (list (same? (first again) held) (string (first again)) text)"
    );
    let values = evaluate(&source, None, &mut NoRequests, &mut Vec::new())?;
    let [Value::List(items)] = values.as_slice() else {
        return Err("no list".into());
    };
    let said: Vec<String> = items.items().iter().filter_map(|v| v.text().ok()).collect();
    assert_eq!(said.first().map(String::as_str), Some("#true"), "{said:?}");
    assert_eq!(said.get(1).map(String::as_str), Some("%3"), "{said:?}");
    assert!(said.get(2).is_some_and(|t| t.contains(":3-4")), "{said:?}");
    // Held in a client of another server run, it is refused.
    let other: Run = "5-6".parse().map_err(|()| "run")?;
    let context = Context {
        of: Some(other),
        ..Context::default()
    };
    let mut out = Vec::new();
    let mut host = Host::new(context, None, &mut out);
    let mut machine = Machine::source("test", &format!("(zoom (quote! %3:{run}))"));
    let failed = run_program(&mut machine, &mut host)
        .err()
        .ok_or("another run's handle got through")?;
    assert!(failed.message.contains("different server runs"), "{failed}");
    Ok(())
}

fn run_program(machine: &mut Machine, host: &mut Host<'_>) -> Result<Vec<Value<Fux>>, Failed> {
    run(machine, host, &mut NoRequests, None)
}

/// A library loads on first use, so nothing marks its code as a library's:
/// a failure in it still names the program's line, not the library's.
#[test]
fn a_failure_in_a_library_names_the_users_line() -> TestResult {
    let source = "\n\n(assert-kind 1 \"pane\" \"a pane\")";
    let failed = evaluate(source, None, &mut NoRequests, &mut Vec::new())
        .err()
        .ok_or("the library took it")?;
    assert!(failed.to_string().contains("test:3"), "{failed}");
    assert!(!failed.to_string().contains("library.grift"), "{failed}");
    Ok(())
}

/// A server's answer a program typed names another run's pane, or a version
/// of one: the run check sees through the record to it.
#[test]
fn a_typed_answer_of_another_run_is_refused() -> TestResult {
    struct Counted(usize);
    impl Asks for Counted {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            if let Some(found) = testing::found(asked) {
                return Ok(found);
            }
            self.0 = self.0.saturating_add(1);
            Ok(Body::Empty.into())
        }
    }
    for value in [
        r#"((kind "answer") (pane %3:3-4))"#,
        r#"((kind "answer") (pane %3:3-4) (run "5-6"))"#,
        r#"((kind "answer") (version "%3:0.0.0.0.0:3-4"))"#,
    ] {
        let context = Context {
            of: Some("5-6".parse().map_err(|()| "a run")?),
            ..Context::default()
        };
        let mut out = Vec::new();
        let mut host = Host::new(context, None, &mut out);
        let mut asks = Counted(0);
        let mut machine = Machine::source("key", &format!("(kill (quote! {value}))"));
        let failed = run(&mut machine, &mut host, &mut asks, None)
            .err()
            .ok_or("another run's pane got through")?;
        assert!(failed.message.contains("different server runs"), "{failed}");
        assert_eq!(asks.0, 0, "{value}");
    }
    // Inside a word's own values: an arrangement's leaves.
    let leaf = r#"(quote! ((kind "answer") (pane %3:3-4)))"#;
    let source =
        format!(r#"(arrange (tab 1) "@1:0:5-6" (list (list "horizontal" 1 1 1 2) {leaf} {leaf}))"#);
    let context = Context {
        of: Some("5-6".parse().map_err(|()| "a run")?),
        ..Context::default()
    };
    let mut out = Vec::new();
    let mut host = Host::new(context, None, &mut out);
    let mut asks = Counted(0);
    let mut machine = Machine::source("key", &source);
    let failed = run(&mut machine, &mut host, &mut asks, None)
        .err()
        .ok_or("another run's leaf got through")?;
    assert!(failed.message.contains("different server runs"), "{failed}");
    assert_eq!(asks.0, 0);
    Ok(())
}

/// The latest step a failed run took is told however many came before, and
/// the list stays one size.
#[test]
fn a_long_run_tells_its_first_and_latest_steps() -> TestResult {
    struct Done;
    impl Asks for Done {
        fn ask(&mut self, asked: &Asked) -> Result<Reply, Failed> {
            Ok(testing::found(asked).unwrap_or_else(|| Body::Empty.into()))
        }
    }
    for steps in [66, 100] {
        let renames: String = (1..=steps)
            .map(|n| format!("(rename (pane 1) \"n{n}\")"))
            .collect();
        let mut out = Vec::new();
        let mut host = Host::new(Context::default(), None, &mut out);
        let mut machine = Machine::source("test", &format!("{renames} (nosuch)"));
        let failed = run(&mut machine, &mut host, &mut Done, None)
            .err()
            .ok_or("nosuch ran")?;
        let said = failed.actions.join("\n");
        let latest = format!("\"n{steps}\")");
        assert!(
            said.contains(r#""n1")"#) && said.contains(&latest),
            "{said}"
        );
        // What it keeps and what it says went are every step.
        let gone = steps + 1 - failed.actions.len();
        assert!(said.contains(&format!("and {gone} more")), "{said}");
        assert!(failed.actions.len() <= 66, "{said}");
    }
    Ok(())
}

/// A kind fux has no code for is no failure of fux's: it is the program's,
/// and keeps its name. One fux has is that code.
#[test]
fn an_unknown_kind_is_the_programs_failure() {
    let raised = |kind: &str| {
        let source = format!("(raise (error (quote! {kind})))");
        evaluate(&source, None, &mut NoRequests, &mut Vec::new()).err()
    };
    let failed = raised("nosuch");
    assert_eq!(failed.as_ref().map(|f| f.code), Some(Code::Fail));
    assert_eq!(failed.and_then(|f| f.kind).as_deref(), Some("nosuch"));
    assert_eq!(raised("closed").map(|f| f.code), Some(Code::Closed));
}

/// A list stays a list though it looks like a record, and what a program
/// leaves prints as data that reads back whole.
#[test]
fn lists_and_left_values_keep_what_they_are() -> TestResult {
    let source = r#"(list (quote! (x 1)) (axis (pane 3) "horizontal"))"#;
    let source = format!("(data-text {source})");
    let values = evaluate(&source, None, &mut NoRequests, &mut Vec::new())?;
    let said: Vec<String> = values.iter().filter_map(|v| v.text().ok()).collect();
    assert!(said.concat().contains("(x 1)"), "{said:?}");
    let source = r#"(list (list (quote! handler) (code! (lambda! () #inert)))
        (list (quote! pending) (axis (pane 3) "horizontal")) (list (quote! run) "1-2"))"#;
    let values = evaluate(source, None, &mut NoRequests, &mut Vec::new())?;
    let [value] = values.as_slice() else {
        return Err("one value".into());
    };
    let text = value.text().map_err(|f| f.message)?;
    assert!(text.contains("(code! (lambda! () #inert))"), "{text}");
    assert!(text.contains(r#"(axis "horizontal"))"#), "{text}");
    Ok(())
}

/// Reading a request repeats its words' checks: an extra value is refused
/// there, as it is in the call.
#[test]
fn a_request_read_back_takes_what_its_words_take() {
    for source in [
        r#"(kind (quote! (request! none (name "kept" "discarded"))))"#,
        r#"(name (options) "kept" "discarded")"#,
    ] {
        let failed = evaluate(source, None, &mut NoRequests, &mut Vec::new());
        assert!(
            failed.is_err_and(|f| f.kind.as_deref() == Some("arity-error")),
            "{source}"
        );
    }
}

/// A word's wrong count is the arity-error a primitive's is: its one value
/// the arguments as a list.
#[test]
fn a_words_wrong_count_fails_as_a_primitives_does() -> Result<(), Box<dyn std::error::Error>> {
    let caught = |call: &str| {
        let source = format!("(catch! {call} e (evaluate-in! values e))");
        let values =
            evaluate(&source, None, &mut NoRequests, &mut Vec::new()).map_err(|f| f.message)?;
        values
            .iter()
            .map(|v| v.text().map_err(|f| f.message))
            .collect::<Result<Vec<_>, _>>()
    };
    assert_eq!(caught("(string 1 2)")?, caught("(first 1 2)")?);
    Ok(())
}

/// fux's own host functions fail as a primitive does: a wrong count is an
/// arity-error, a wrong type a type-error carrying the one value of the
/// wrong type, and a library number fux has no library for a range-error
/// carrying that number.
#[test]
fn host_functions_fail_as_primitives_do() -> Result<(), Box<dyn std::error::Error>> {
    let caught = |call: &str| {
        let source = format!("(catch! {call} e (evaluate-in! values e))");
        let values =
            evaluate(&source, None, &mut NoRequests, &mut Vec::new()).map_err(|f| f.message)?;
        values
            .iter()
            .map(|v| v.text().map_err(|f| f.message))
            .collect::<Result<Vec<_>, String>>()
            .map(|texts| texts.concat())
    };
    assert_eq!(caught("(native-load-library 1 2)")?, caught("(first 1 2)")?);
    assert_eq!(caught("(native-load-library)")?, caught("(first)")?);
    assert_eq!(caught(r#"(read-source 1 #false)"#)?, caught("(first 1)")?);
    assert_eq!(caught("(native-load-library 99)")?, "(99)");
    let kind = |call: &str| {
        let failed = evaluate(call, None, &mut NoRequests, &mut Vec::new()).err();
        failed.and_then(|f| f.kind)
    };
    assert_eq!(kind("(native-load-library)").as_deref(), Some("arity-error"));
    assert_eq!(kind("(read-source 1 #false)").as_deref(), Some("type-error"));
    assert_eq!(kind("(native-load-library 99)").as_deref(), Some("range-error"));
    assert_eq!(kind("(native-load-library -1)").as_deref(), Some("range-error"));
    Ok(())
}

/// A code value that holds no form is a bad value, as one that doesn't
/// read is where fux takes one in, and no parse error with nothing to say.
#[test]
fn a_code_value_with_no_form_is_a_bad_value() -> TestResult {
    let program = crate::screens::Program::Code(fux_wire::callback::Code {
        source: "(lambda! (c) c)".into(),
        place: None,
    });
    let empty = fux_wire::callback::Code {
        source: String::new(),
        place: None,
    };
    let args = [fux_wire::callback::Data::Code(empty)];
    let mut machine = callback_machine(None, &program, Some(&args), None)?;
    let mut out = Vec::new();
    let mut host = Host::new(Context::default(), None, &mut out);
    let failed = run(&mut machine, &mut host, &mut NoRequests, None)
        .err()
        .ok_or("the callback ran")?;
    assert_eq!(failed.code, Code::BadValue, "{failed}");
    Ok(())
}

/// A stop as a program's thread sees it, held up `at` a step of it. With
/// `word` it comes as the program's first word begins; else as the program
/// first prints. Says how many requests reached the server, what the run
/// came to, and asks over a real connection, as a stop shuts one.
#[cfg(unix)]
fn stopped_at(at: Step, program: &str, word: bool) -> Result<(usize, Failed), String> {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    const PATIENCE: Duration = Duration::from_secs(60);
    struct Begins(mpsc::Sender<()>);
    impl Write for Begins {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (begin, begun) = mpsc::channel();
    let (held, holding) = mpsc::channel();
    let (arrive, arrival) = mpsc::channel();
    let (end, ended) = mpsc::channel();
    let arrival = Mutex::new(arrival);
    let holding = Mutex::new(holding);
    let (first, waited) = (std::sync::atomic::AtomicBool::new(word), arrive.clone());
    let begins = begin.clone();
    let stop = Stop::pausing(move |step| {
        if step == at {
            let _ = held.send(());
            let _ = arrival.lock().map(|a| a.recv_timeout(PATIENCE));
        } else if step == Step::Looks && first.swap(false, Ordering::SeqCst) {
            let _ = begins.send(());
            let _ = holding.lock().map(|h| h.recv_timeout(PATIENCE));
        } else if step == Step::Waits {
            let _ = waited.send(());
        }
    });
    let stopper = stop.clone();
    let stopping = std::thread::spawn(move || {
        let _ = begun.recv_timeout(PATIENCE);
        stopper.stop();
        let _ = end.send(());
    });
    let (mine, peer) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
    // Answers once the stop has ended, so a stop that shuts a request shuts
    // it before its answer.
    let answering = std::thread::spawn(move || {
        use fux_wire::protocol::Frame;
        let mut peer = peer;
        let (mut decoder, mut bytes) = (fux_wire::protocol::Decoder::default(), [0; 4096]);
        let mut asked = 0_usize;
        while let Ok(read @ 1..) = peer.read(&mut bytes) {
            decoder.push(bytes.get(..read).unwrap_or_default());
            while let Ok(Some(request)) = decoder.frame::<Asked>() {
                asked = asked.saturating_add(1);
                let _ = arrive.send(());
                let _ = ended.recv_timeout(PATIENCE);
                let reply: Reply = testing::found(&request).unwrap_or_else(|| Body::Empty.into());
                let mut out = fux_vt::bytes::ByteQueue::default();
                let encoded = fux_wire::wire::encode(&reply);
                fux_wire::protocol::split_into(
                    &encoded,
                    fux_wire::protocol::ToCommand::Part,
                    &mut out,
                );
                out.push_with(|frames| fux_wire::protocol::ToCommand::PartsEnd.encode_into(frames));
                let _ = peer.write_all(out.as_slice());
            }
        }
        asked
    });
    let mut asks = Server {
        requests: Some(crate::client::Requests::over(mine)),
        stop: stop.clone(),
        ..Server::default()
    };
    let mut out = Begins(begin);
    let mut host = Host::new(Context::default(), None, &mut out);
    let mut machine = Machine::source("stopped", program);
    let result = run(&mut machine, &mut host, &mut asks, Some(&stop));
    drop(asks);
    stopping.join().map_err(|_| "the stopper panicked")?;
    let asked = answering.join().map_err(|_| "the server panicked")?;
    Ok((asked, result.err().ok_or("a stopped run ended well")?))
}

/// However a stop is held up, a program's thread never finds it half done: a
/// body's word is refused before it asks, and a cleanup's requests all reach
/// the server, none shut by the stop that let them run.
#[cfg(unix)]
#[test]
fn a_stop_is_one_step_to_the_program() -> TestResult {
    for at in [Step::Told, Step::Noted] {
        let (asked, _) = stopped_at(at, r#"(rename (pane 1) "went-on")"#, true)?;
        assert_eq!(asked, 0);
        let cleaned = r#"(define! spin (lambda! () (spin)))
            (finally! (sequence! (display "go") (spin)) (rename (pane 1) "cleaned"))"#;
        let (asked, failed) = stopped_at(at, cleaned, false)?;
        assert_eq!(asked, 2, "{failed}");
        assert!(failed.actions.iter().any(|step| step.contains("cleaned")));
    }
    Ok(())
}

/// A stop grift has already acted on stops no walk of a cleanup's: however a
/// stop is held up, what a cleanup asks about a value still answers.
#[cfg(unix)]
#[test]
fn a_cleanups_walk_goes_on_after_the_stop_that_let_it_run() -> TestResult {
    for at in [Step::Told, Step::Noted] {
        let cleaned = r#"(define! spin (lambda! () (spin)))
            (finally! (sequence! (display "go") (spin))
                (sequence! (define! asked (kind (list 1 2))) (rename (pane 1) asked)))"#;
        let (asked, failed) = stopped_at(at, cleaned, false)?;
        assert_eq!(asked, 2, "{failed}");
        assert!(failed.actions.iter().any(|step| step.contains("rename")));
    }
    Ok(())
}
