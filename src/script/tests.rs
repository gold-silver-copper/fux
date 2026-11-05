use super::*;
use crate::TestResult;
struct NoRequests;
impl Asks for NoRequests {
    fn ask(&mut self, _: &Asked) -> Result<Reply, Failed> {
        Err(Failed::new(Code::Client, "unexpected request"))
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
    let source = "(version \"%1:0.7.0.0.0:1-1\")";
    let values = testing::evaluate(source, None, &mut NoRequests, &mut Vec::new())?;
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
    let path = std::path::PathBuf::from(std::env::var_os("TMPDIR").ok_or("TMPDIR")?)
        .join(format!("default-keys-{}.grift", std::process::id()));
    std::fs::write(&path, &source)?;
    let state = run_file(&path);
    std::fs::remove_file(path)?;
    assert_eq!(state?.keys, defaults.keys);
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
            "(sequence! (define! variables-6 (context)) (bind variables-6 (quote! held) (wait (timeout (pane 1) 0))) (define! value-10 (evaluate (quote! held) variables-6)) \"exited\" (define! value-12 (get value-10 \"exited\")) (define! value-13 (evaluate (quote! held) variables-6)) \"signal\" (list value-12 (get value-13 \"signal\")))",
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
        "(sequence! (define! variables-30 (context)) (bind variables-30 (quote! a) (list-session)) (bind variables-30 (quote! b) (list-session)) (print (datum (list (evaluate (quote! a) variables-30) (evaluate (quote! b) variables-30)))) #inert)",
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
    assert!(out.starts_with(b"((id %1)") && out.ends_with(b")\n"));
    Ok(())
}
