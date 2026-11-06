//! vt-esc-lint [--all] [--rows N --cols N] [FILE]: lists each control
//! sequence in FILE (or stdin), flags malformed and unknown ones, and says
//! what the stream leaves set. Exits 1 if anything was flagged.
use std::io::{Read, Write};
use std::process::ExitCode;
use vt_esc_lint::{Flag, lint};

/// Shows bytes as their names: `ESC [ 1 m`, controls as `^J`.
fn show(bytes: &[u8]) -> String {
    let mut out = String::new();
    let text = String::from_utf8_lossy(bytes);
    for c in text.chars() {
        match c {
            '\x1b' => out.push_str("ESC "),
            '\x07' => out.push_str("BEL"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("^{}", char::from(b'@' + u8::try_from(u32::from(c)).unwrap_or(0)))),
            '\x7f' => out.push_str("DEL"),
            c => out.push(c),
        }
    }
    if out.chars().count() > 40 {
        out = format!("{}…", out.chars().take(39).collect::<String>());
    }
    out
}

fn run() -> Result<bool, String> {
    let mut args = std::env::args().skip(1);
    let (mut all, mut file, mut rows, mut cols) = (false, None, 24u16, 80u16);
    while let Some(arg) = args.next() {
        let mut number = |what: &str| -> Result<u16, String> {
            args.next().and_then(|n| n.parse().ok()).ok_or(format!("{what} wants a number"))
        };
        match arg.as_str() {
            "--all" => all = true,
            "--rows" => rows = number("--rows")?,
            "--cols" => cols = number("--cols")?,
            "-h" | "--help" => {
                return Err("usage: vt-esc-lint [--all] [--rows N] [--cols N] [FILE|-]\n  --all also lists text runs".into());
            }
            _ if file.is_none() => file = Some(arg),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    let mut bytes = Vec::new();
    match file.as_deref() {
        None | Some("-") => std::io::stdin().read_to_end(&mut bytes).map(drop),
        Some(path) => std::fs::read(path).map(|b| bytes = b),
    }
    .map_err(|e| format!("reading {}: {e}", file.as_deref().unwrap_or("stdin")))?;
    let size = fux_vt::Size::new(rows, cols).map_err(|_| "rows and columns must be 1 to 65535")?;
    let report = lint(&bytes, size).map_err(|e| e.to_string())?;

    let mut out = std::io::stdout().lock();
    let mut write = |line: String| writeln!(out, "{line}").map_err(|e| format!("stdout: {e}"));
    for f in &report.findings {
        if f.text && f.flag.is_none() && !all {
            continue;
        }
        let raw = bytes.get(f.start..f.end).unwrap_or_default();
        let mark = match &f.flag {
            None => "  ",
            Some(Flag::Malformed(_)) => "!!",
            Some(Flag::Unknown(_)) => "??",
        };
        let why = match &f.flag {
            Some(Flag::Malformed(why) | Flag::Unknown(why)) if *why != f.meaning => format!(" ({why})"),
            _ => String::new(),
        };
        write(format!("{:>7}  {mark} {:<40}  {}{why}", f.start, show(raw), f.meaning))?;
    }
    let sequences = report.findings.iter().filter(|f| !f.text).count();
    let (malformed, unknown) = (report.count(true), report.count(false));
    write(format!(
        "\n{} bytes, {sequences} controls and sequences: {malformed} malformed (!!), {unknown} unknown (??)",
        bytes.len()
    ))?;
    write("At the end:".into())?;
    for line in &report.left {
        write(format!("  {line}"))?;
    }
    Ok(malformed + unknown == 0)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprintln!("vt-esc-lint: {message}");
            ExitCode::from(2)
        }
    }
}
