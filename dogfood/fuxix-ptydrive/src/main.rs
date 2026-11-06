//! fuxix-ptydrive [--size ROWSxCOLS] [--script FILE] [--out FILE] [--exit-wait MS] -- PROGRAM ARGS...
//!
//! Starts PROGRAM on a PTY, runs the script (from FILE, or stdin), waits up
//! to --exit-wait for it to end (then hangs it up, then kills it), and writes
//! every byte it output to --out or stdout. A log of the steps goes to stderr.
//! Exits 0, 1 if a `wait` step did not find its text, 2 on bad usage, 3 on
//! a failure to run.
use fuxix_ptydrive::Driver;
use fuxix_ptydrive::script::{self, Outcome};
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

struct Options {
    rows: u16,
    cols: u16,
    script: Option<String>,
    out: Option<String>,
    exit_wait: Duration,
    argv: Vec<String>,
}

const USAGE: &str = "usage: fuxix-ptydrive [--size ROWSxCOLS] [--script FILE] [--out FILE] [--exit-wait MS] -- PROGRAM ARGS...";

fn options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        rows: 24,
        cols: 80,
        script: None,
        out: None,
        exit_wait: Duration::from_millis(3000),
        argv: Vec::new(),
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} wants a value"));
        match arg.as_str() {
            "--size" => {
                let value = value()?;
                let (rows, cols) = value.split_once('x').ok_or("--size wants ROWSxCOLS")?;
                options.rows = rows.parse().map_err(|_| format!("bad rows {rows:?}"))?;
                options.cols = cols.parse().map_err(|_| format!("bad columns {cols:?}"))?;
            }
            "--script" => options.script = Some(value()?),
            "--out" => options.out = Some(value()?),
            "--exit-wait" => {
                let value = value()?;
                let ms = value
                    .parse()
                    .map_err(|_| format!("bad --exit-wait {value:?}"))?;
                options.exit_wait = Duration::from_millis(ms);
            }
            "--" => {
                options.argv = args.collect();
                break;
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    fuxix_ptydrive::size(options.rows, options.cols).map_err(|e| e.to_string())?;
    if options.argv.is_empty() {
        return Err("no program to run".into());
    }
    Ok(options)
}

fn main() -> ExitCode {
    let options = match options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(reason) => {
            if !reason.is_empty() {
                eprintln!("fuxix-ptydrive: {reason}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match drive(&options) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("fuxix-ptydrive: {error}");
            ExitCode::from(3)
        }
    }
}

/// Runs it all: whether every `wait` found its text.
fn drive(options: &Options) -> Result<bool, Box<dyn std::error::Error>> {
    let text = match &options.script {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
    };
    let steps = script::parse(&text)?;
    let mut driver = Driver::spawn(&options.argv, options.rows, options.cols)
        .map_err(|e| format!("{}: {e}", options.argv.join(" ")))?;
    let mut mark = 0;
    let mut all_found = true;
    for (index, step) in steps.iter().enumerate() {
        let before = driver.output().len();
        let outcome = script::run(&mut driver, step, &mut mark)?;
        let read = driver.output().len().saturating_sub(before);
        eprintln!(
            "step {}: {step} -> {outcome:?} (+{read} bytes)",
            index.saturating_add(1)
        );
        all_found &= !matches!(outcome, Outcome::NotFound);
    }
    let status = driver.finish(options.exit_wait)?;
    eprintln!("ended: {status:?}, {} bytes", driver.output().len());
    match &options.out {
        Some(path) => std::fs::write(path, driver.output()).map_err(|e| format!("{path}: {e}"))?,
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(driver.output())?;
            stdout.flush()?;
        }
    }
    Ok(all_found)
}
