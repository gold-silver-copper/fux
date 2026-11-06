//! vt-web-backend [--size ROWSxCOLS] [--chunk BYTES] [--history BYTES]
//!
//! Reads a program's output on stdin and writes, after each chunk read (or
//! each --chunk bytes of it), the JSON lines a browser paints the terminal
//! from (see the library's docs). At the end of input an update still held
//! for a synchronized frame is sent.
use std::io::{Read, Write};
use std::process::ExitCode;
use vt_web_backend::Backend;

const USAGE: &str =
    "usage: vt-web-backend [--size ROWSxCOLS] [--chunk BYTES] [--history BYTES] < output";

struct Options {
    rows: u16,
    cols: u16,
    chunk: usize,
    history: usize,
}

fn options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        rows: 24,
        cols: 80,
        chunk: 65536,
        history: 1 << 20,
    };
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} wants a value"))?;
        let number = |text: &str| {
            text.parse::<usize>()
                .map_err(|_| format!("bad {arg} {text:?}"))
        };
        match arg.as_str() {
            "--size" => {
                let (rows, cols) = value.split_once('x').ok_or("--size wants ROWSxCOLS")?;
                options.rows = rows.parse().map_err(|_| format!("bad rows {rows:?}"))?;
                options.cols = cols.parse().map_err(|_| format!("bad columns {cols:?}"))?;
            }
            "--chunk" => options.chunk = number(&value)?.max(1),
            "--history" => options.history = number(&value)?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn main() -> ExitCode {
    let options = match options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(reason) => {
            eprintln!("vt-web-backend: {reason}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vt-web-backend: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(options: &Options) -> Result<(), vt_web_backend::Error> {
    let mut backend = Backend::new(options.rows, options.cols, options.history)?;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut buffer = vec![0u8; options.chunk];
    let mut lines = String::new();
    loop {
        let read = match stdin.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        backend.feed(buffer.get(..read).unwrap_or_default(), &mut lines)?;
        stdout.write_all(lines.as_bytes())?;
        lines.clear();
    }
    backend.flush(&mut lines);
    stdout.write_all(lines.as_bytes())?;
    stdout.flush()?;
    Ok(())
}
