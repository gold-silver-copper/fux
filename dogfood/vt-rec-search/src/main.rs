//! vt-rec-search: replays terminal recordings through fux-vt and answers
//! `search WORD` with when and where the word was on screen.
//!
//! `vt-rec-search [--size ROWSxCOLS] [-q COMMAND]... FILE...`
//!
//! Each FILE is an asciicast (v2 or v3) or a raw log of terminal output,
//! which is replayed on a ROWSxCOLS screen (24x80 unless given). Commands,
//! from each `-q` or else from stdin, one a line:
//!   search WORD     each stay of WORD on screen (any case; WORD* for a prefix)
//!   screen N AT     the screen recording N showed at AT (seconds, or bytes for a raw log)
//!   stats           recordings, distinct screens and words indexed
use std::io::{BufRead, Write};
use std::process::ExitCode;
use vt_rec_search::{At, Index, Recording};

fn usage() -> String {
    "usage: vt-rec-search [--size ROWSxCOLS] [-q COMMAND]... FILE...".into()
}

fn run() -> Result<(), String> {
    let mut size = (24, 80);
    let mut queries = Vec::new();
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--size" => {
                let value = args.next().ok_or_else(usage)?;
                let (rows, cols) = value.split_once('x').ok_or_else(usage)?;
                size = (
                    rows.parse().map_err(|_| usage())?,
                    cols.parse().map_err(|_| usage())?,
                );
            }
            "-q" => queries.push(args.next().ok_or_else(usage)?),
            "-h" | "--help" => return Err(usage()),
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        return Err(usage());
    }
    let mut index = Index::default();
    for file in &files {
        let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        let recording = Recording::read(bytes).map_err(|e| format!("{file}: {e}"))?;
        index.add(file, &recording, size)?;
    }
    let mut out = std::io::stdout().lock();
    let mut answer = |line: &str| -> Result<(), String> {
        let text = respond(&index, line);
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| e.to_string())
    };
    if queries.is_empty() {
        for line in std::io::stdin().lock().lines() {
            answer(&line.map_err(|e| e.to_string())?)?;
        }
    } else {
        for query in &queries {
            answer(query)?;
        }
    }
    Ok(())
}

/// The answer to one command.
fn respond(index: &Index, line: &str) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (None, ..) => String::new(),
        (Some("search"), Some(word), None) => {
            let hits = index.search(word);
            let mut text = format!("{} on screen {} time(s)\n", word, hits.len());
            for hit in hits {
                let file = index.files.get(hit.file).map_or("?", String::as_str);
                text.push_str(&format!(
                    "{file}  {} - {}  row {} col {}  | {}\n",
                    hit.from, hit.until, hit.row, hit.col, hit.line
                ));
            }
            text
        }
        (Some("screen"), Some(n), Some(at)) => {
            let Ok(file) = n.parse::<usize>() else {
                return "screen N AT: N is a number\n".into();
            };
            let raw = index
                .states
                .get(file)
                .and_then(|s| s.first())
                .is_some_and(|s| matches!(s.at, At::Byte(_)));
            let at = match raw {
                true => at.parse().map(At::Byte).ok(),
                false => at.trim_end_matches('s').parse().map(At::Secs).ok(),
            };
            match at.and_then(|at| index.screen_at(file, at)) {
                Some(state) => {
                    let mut text = format!("from {}:\n", state.at);
                    for row in &state.rows {
                        text.push_str(&format!("|{row}\n"));
                    }
                    text
                }
                None => "no such recording or time\n".into(),
            }
        }
        (Some("stats"), None, _) => {
            let screens: usize = index.states.iter().map(Vec::len).sum();
            format!(
                "{} recording(s), {screens} distinct screens, {} words\n",
                index.files.len(),
                index.word_count()
            )
        }
        _ => format!("unknown command {line:?}: search WORD | screen N AT | stats\n"),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vt-rec-search: {message}");
            ExitCode::FAILURE
        }
    }
}
