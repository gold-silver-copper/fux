//! The example programs in `tests/examples`: each says in its header what
//! it leaves (`# expect: …`), the error it fails with as it runs
//! (`# fails: CODE`), or the error that refuses it before it runs
//! (`# refused: CODE at LINE`).
mod common;
use common::Toy;
use fuxth::{Compiler, Error, Machine, Outcome, Place, STDLIB, Vocabulary, token};

/// How a program ended.
#[derive(Debug, PartialEq)]
enum Ended {
    Left(String),
    Failed(String),
    Refused(String, u32),
}

fn run(name: &str, text: &str) -> Ended {
    let refused = |e: Error| {
        let line = match e.place {
            Some(Place::Line { line, .. }) => line,
            Some(Place::Argument(_)) | None => 0,
        };
        Ended::Refused(e.code.name().to_owned(), line)
    };
    let mut compiler = Compiler::new(Vocabulary::default());
    let compiled = token::source("stdlib.fux", STDLIB)
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| token::source(name, text))
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| compiler.finish());
    let program = match compiled {
        Ok(program) => program,
        Err(e) => return refused(e),
    };
    let mut machine = Machine::new(program);
    let mut host = Toy::default();
    match machine.run(&mut host, 50_000_000) {
        Ok(Outcome::Finished) => {
            let left: Vec<String> = (host.printed.iter().cloned())
                .chain(
                    machine
                        .stack()
                        .iter()
                        .map(|v| v.text().unwrap_or_else(|f| f.message)),
                )
                .collect();
            Ended::Left(left.join(" "))
        }
        Ok(Outcome::Paused | Outcome::Suspended(_)) => Ended::Failed("steps".to_owned()),
        Err(e) => Ended::Failed(e.code.name().to_owned()),
    }
}

/// What the header says it ends with.
fn expected(text: &str) -> Option<Ended> {
    text.lines().find_map(|line| {
        let line = line.strip_prefix("# ")?;
        if let Some(left) = line.strip_prefix("expect: ") {
            return Some(Ended::Left(left.trim().to_owned()));
        }
        if let Some(code) = line.strip_prefix("fails: ") {
            return Some(Ended::Failed(code.trim().to_owned()));
        }
        let refused = line.strip_prefix("refused: ")?;
        let (code, at) = refused.split_once(" at ")?;
        Some(Ended::Refused(code.to_owned(), at.trim().parse().ok()?))
    })
}

#[test]
fn every_example_ends_as_its_header_says() -> std::io::Result<()> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/examples");
    let mut names: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "fux"))
        .collect();
    names.sort();
    assert!(names.len() >= 20, "{} examples", names.len());
    for path in &names {
        let text = std::fs::read_to_string(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(Some(run(&name, &text)), expected(&text), "{name}");
    }
    Ok(())
}
