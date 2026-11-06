//! vt-tui-oracle: replays a recording of a TUI's output and checks a file
//! of assertions against the screen it leaves. Exit 0 if all hold, 1 if
//! any fails, 2 if the files can't be read.
use std::io::Write;
use std::process::ExitCode;
use vt_tui_oracle::{Replay, parse};

const USAGE: &str = "usage: vt-tui-oracle [--size ROWSxCOLS] [--script] [--dump] RECORDING ASSERTIONS
  --size ROWSxCOLS  the terminal the program wrote to (24x80)
  --script          RECORDING is script(1)'s typescript: drop its first line
                    (\"Script started on ...\") and a \"Script done\" line at the end
  --dump            print the screen, also when every assertion holds
assertions, one a line, rows and columns from 0:
  cell ROW COL TEXT | cursor ROW COL | mode NAME on|off | title TEXT | fg ROW COL COLOR";

struct Args {
    size: fux_vt::Size,
    script: bool,
    dump: bool,
    recording: String,
    assertions: String,
}

fn args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut size = (24, 80);
    let (mut script, mut dump, mut files) = (false, false, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--size" => {
                let value = args.next().ok_or("--size needs ROWSxCOLS")?;
                let parsed = value.split_once('x').and_then(|(r, c)| Some((r.parse().ok()?, c.parse().ok()?)));
                size = parsed.ok_or(format!("--size {value}: not ROWSxCOLS"))?;
            }
            "--script" => script = true,
            "--dump" => dump = true,
            "-h" | "--help" => return Err("help".into()),
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => files.push(arg),
        }
    }
    let size = fux_vt::Size::new(size.0, size.1).map_err(|e| format!("--size: {e}"))?;
    let [recording, assertions] = <[String; 2]>::try_from(files)
        .map_err(|_| "give a recording and an assertions file".to_owned())?;
    Ok(Args { size, script, dump, recording, assertions })
}

/// A typescript's output, without the lines script(1) adds around it.
fn unscript(bytes: &[u8]) -> &[u8] {
    let body = match bytes.strip_prefix(b"Script started on ") {
        Some(rest) => rest.iter().position(|&b| b == b'\n').and_then(|n| rest.get(n.saturating_add(1)..)),
        None => None,
    };
    let body = body.unwrap_or(bytes);
    let done = body.windows(13).rposition(|w| w == b"\nScript done ");
    done.and_then(|at| body.get(..at)).unwrap_or(body)
}

fn main() -> ExitCode {
    let mut err = std::io::stderr().lock();
    let args = match args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            let _ = writeln!(err, "vt-tui-oracle: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let read = |path: &str| std::fs::read(path).map_err(|e| format!("{path}: {e}"));
    let loaded = read(&args.recording).and_then(|recording| {
        let text = read(&args.assertions)?;
        let text = String::from_utf8(text).map_err(|_| format!("{}: not UTF-8", args.assertions))?;
        let lines = parse(&text).map_err(|e| format!("{}: {e}", args.assertions))?;
        let bytes = if args.script { unscript(&recording) } else { &recording };
        let replay = Replay::new(bytes, args.size).map_err(|e| format!("{}: {e}", args.recording))?;
        Ok((lines, replay))
    });
    let (lines, replay) = match loaded {
        Ok(loaded) => loaded,
        Err(message) => {
            let _ = writeln!(err, "vt-tui-oracle: {message}");
            return ExitCode::from(2);
        }
    };
    let mut out = std::io::stdout().lock();
    let mut failed = 0usize;
    for line in &lines {
        if let Err(got) = replay.check(&line.assertion) {
            failed = failed.saturating_add(1);
            let _ = writeln!(out, "FAIL {}:{}: {}\n     got {got}", args.assertions, line.number, line.source);
        }
    }
    let passed = lines.len().saturating_sub(failed);
    if failed > 0 || args.dump {
        let _ = write!(out, "{}", replay.dump());
    }
    let _ = writeln!(out, "{passed} passed, {failed} failed");
    match failed {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
