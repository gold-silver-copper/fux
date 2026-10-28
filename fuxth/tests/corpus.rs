//! A corpus of programs from outside the repo, held to the same checks as
//! the random ones: `FUXTH_CORPUS=DIR cargo test -p fuxth --test corpus`
//! runs every `*.fux` under DIR (in folders, as `program.fux`) whole, a
//! step at a time and 7 at a time, without changing their observable result.
//! Without the variable it does nothing.
mod common;
use common::{Ended, fuxth};

#[test]
fn a_corpus_ends_the_same_every_way() -> std::io::Result<()> {
    let Ok(dir) = std::env::var("FUXTH_CORPUS") else {
        return Ok(());
    };
    let mut files = Vec::new();
    let mut todo = vec![std::path::PathBuf::from(dir)];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|e| e == "fux") {
                files.push(path);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no programs under the corpus");
    let mut wrong = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let whole = fuxth(&text, u64::MAX);
        let name = file.display();
        if whole != Ended::Endless {
            for budget in [1, 7] {
                let paused = fuxth(&text, budget);
                if paused != Ended::Endless && paused != whole {
                    wrong.push(format!(
                        "{name}: budget {budget}: {paused:?} against {whole:?}"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {}:\n{}",
        wrong.len(),
        files.len(),
        wrong.join("\n")
    );
    Ok(())
}
