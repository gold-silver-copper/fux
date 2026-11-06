use fuxix_portable_runner::{os, parse, run, summary};
use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "usage: fuxix-portable-runner TASKFILE

Runs each task of TASKFILE in turn. A line is NAME TIMEOUT MODE COMMAND...,
TIMEOUT like 30s, 2.5s, 500ms or 2m, MODE pipe (stdout and stderr on one
pipe, stdin empty) or pty (a 24x80 terminal); '...' and \"...\" quote, # starts
a comment. A task out of time has its whole process tree ended (TERM, then
KILL after 0.5 s), as does anything a task leaves running. Output is shown
as it comes, each line after its task's name; a summary ends the run. Exits
1 if any task did not exit 0.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if path == "-h" || path == "--help" {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let specs = match std::fs::read_to_string(path)
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|text| parse(&text).map_err(|e| format!("{path}: {e}")))
    {
        Ok(specs) => specs,
        Err(error) => {
            eprintln!("fuxix-portable-runner: {error}");
            return ExitCode::from(2);
        }
    };
    let mut os = match os::Os::new() {
        Ok(os) => os,
        Err(error) => {
            eprintln!("fuxix-portable-runner: {error}");
            return ExitCode::from(2);
        }
    };
    let mut reports = Vec::new();
    for spec in &specs {
        let stdout = std::io::stdout();
        let mut line_start = true;
        let mut show = |bytes: &[u8]| {
            let mut out = stdout.lock();
            for piece in bytes.split_inclusive(|&b| b == b'\n') {
                if line_start {
                    let _ = write!(out, "{:>12} | ", spec.name);
                }
                let _ = out.write_all(piece);
                line_start = piece.ends_with(b"\n");
            }
            let _ = out.flush();
        };
        let report = run(&mut os, spec, &mut show);
        if !line_start {
            println!();
        }
        reports.push(report);
    }
    println!("---");
    for report in &reports {
        println!("{}", summary(report));
    }
    if reports.iter().all(|r| r.ok()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
