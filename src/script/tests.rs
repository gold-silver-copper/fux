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
