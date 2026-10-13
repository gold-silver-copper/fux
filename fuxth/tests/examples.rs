//! The example programs in `tests/examples`: each says in its header what
//! it leaves (`# expect: …`), the error it fails with as it runs
//! (`# fails: CODE`), or the error that refuses it before it runs
//! (`# refused: CODE at LINE`).
use fuxth::{
    AppValue, Args, Compiler, Done, Error, Failure, Host, Limits, Machine, Outcome, PRELUDE,
    Place, Value, Vocabulary, token,
};

#[derive(Clone, Debug, PartialEq)]
struct Nothing;

impl AppValue for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// A host with no words.
struct Bare;

impl Host for Bare {
    type Value = Nothing;
    type Request = ();
    fn call(
        &mut self,
        _word: usize,
        _args: Args<'_, Nothing>,
        _out: &mut Vec<Value<Nothing>>,
    ) -> Result<Done<()>, Failure> {
        Err(Failure::new("no_words", "this host has no words"))
    }
}

/// How a program ended.
#[derive(Debug, PartialEq)]
enum Ended {
    Left(String),
    Failed(String),
    Refused(String, u32),
}

fn shown(value: &Value<Nothing>) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.to_string(),
        Value::App(a) => a.kind().to_owned(),
    }
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
    let compiled = token::source("prelude.fux", PRELUDE)
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| token::source(name, text))
        .and_then(|tokens| compiler.add(&tokens))
        .and_then(|()| compiler.finish());
    let program = match compiled {
        Ok(program) => program,
        Err(e) => return refused(e),
    };
    let mut machine = Machine::new(program, Limits::default());
    match machine.run(&mut Bare, 50_000_000) {
        Ok(Outcome::Finished) => {
            let left: Vec<String> = machine.stack().iter().map(shown).collect();
            Ended::Left(left.join(" "))
        }
        Ok(Outcome::Paused | Outcome::Suspended(())) => Ended::Failed("steps".to_owned()),
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
    let mut wrong = Vec::new();
    for path in &names {
        let text = std::fs::read_to_string(path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(expected) = expected(&text) else {
            wrong.push(format!("{name}: no expect, fails or refused line"));
            continue;
        };
        let ended = run(&name, &text);
        if ended != expected {
            wrong.push(format!("{name}: expected {expected:?}, got {ended:?}"));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
    Ok(())
}
