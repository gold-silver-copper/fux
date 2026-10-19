//! Other terminals' fuzz corpora, replayed through fux-vt with its
//! invariant checker on (HAR-71): Ghostty's 4,001 AFL++ inputs and
//! vt100-rust's 37 seeds, vendored under `external/` (both MIT, each with
//! its licence), and mosh's 27 libFuzzer seeds (GPL-3.0, so not vendored),
//! fetched at a pinned commit by the ignored test. `external/SOURCES` says
//! where each came from; each corpus's SHA-256s are its list, which drives
//! the replay. A corpus is read as each fuzzer reads it, as bytes to the
//! terminal, verbatim: Ghostty's leading mode byte (its slice or scalar
//! path) is a byte of output too.
//!
//! Each input is fed to a fresh 24 by 80 parser with every feature on, as
//! all three fuzzers make theirs: whole, in fux's 4 KiB reads, and a byte
//! at a time. The checker (`Parser::check`) and the public API's
//! invariants (`corpus/invariants.rs`) hold after every read, and the
//! three feeds end alike (threats.md section 6). A panic or a fault is a
//! fux-vt defect, reported with the input's path.
//!
//! `cargo test -p fux-vt --features checks --release --test
//! external_corpora`, and `-- --ignored` for mosh's.
#![cfg(feature = "checks")]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;

use fux_vt::{Feature, Options, Parser, Size};

#[path = "corpus/invariants.rs"]
mod invariants;
#[path = "corpus/known.rs"]
mod known;
#[path = "corpus/pieces.rs"]
mod pieces;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// History bytes each parser keeps: enough for scrolling to reach it.
const HISTORY: usize = 1 << 16;

/// mosh's commit the corpus is fetched at, and where from.
const MOSH_COMMIT: &str = "decd9b705eb81626f694335b8d5940538beb06da";

/// Where the vendored corpora are.
fn external() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/external")
}

/// The files `list` names (`sha256sum` lines), relative to its directory.
fn listed(list: &Path) -> Result<Vec<PathBuf>> {
    let dir = list.parent().ok_or("a list has a directory")?;
    let text = std::fs::read_to_string(list)?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(_, name)| dir.join(name))
        .collect())
}

/// Whether `sha256sum` confirms every file of `list`: `Ok(false)` only if
/// there is no `sha256sum` to ask, said on stderr.
fn hashes_match(list: &Path) -> Result<bool> {
    let dir = list.parent().ok_or("a list has a directory")?;
    match Command::new("sha256sum")
        .args(["--check", "--quiet", "--strict"])
        .arg(list)
        .current_dir(dir)
        .status()
    {
        Ok(status) if status.success() => Ok(true),
        Ok(status) => Err(format!("{}: sha256sum --check: {status}", list.display()).into()),
        Err(error) => {
            loud(&format!(
                "no sha256sum ({error}): hashes of {} unchecked",
                list.display()
            ));
            Ok(false)
        }
    }
}

/// Says `text` on stderr, where a skip must show.
fn loud(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "external_corpora: {text}");
}

/// A fresh parser as the three fuzzers make theirs.
fn parser() -> Result<Parser> {
    let options: Options = Feature::ALL.into_iter().collect();
    Ok(Parser::with_options(Size::new(24, 80)?, HISTORY, options)?)
}

/// `bytes` fed in pieces of `size`, the invariants checked after each one
/// if `every` says so, else at the end.
fn fed(bytes: &[u8], size: usize, every: bool) -> Result<Parser> {
    let mut parser = parser()?;
    for piece in pieces::pieces(bytes, size) {
        // A refusal of room (`Error::Capacity`) costs only what it was
        // for; the rest is read, and the invariants still hold.
        let _ = parser.process(piece);
        if every {
            invariants::check(&parser);
        }
    }
    invariants::check(&parser);
    Ok(parser)
}

/// One input's replay: whole, in 4 KiB reads, a byte at a time.
fn replay(bytes: &[u8]) -> Result {
    let whole = fed(bytes, bytes.len(), true)?;
    let reads = fed(bytes, 4096, true)?;
    invariants::same_feed(&whole, &reads);
    let bytewise = fed(bytes, 1, false)?;
    invariants::same_feed(&whole, &bytewise);
    Ok(())
}

/// Replays every file `list` names, `count` of them: the inputs that
/// panicked or failed a check, each with its message.
fn replay_all(list: &Path, count: usize) -> Result<Vec<String>> {
    let files = listed(list)?;
    assert_eq!(files.len(), count, "{}", list.display());
    let mut failed = Vec::new();
    for file in files {
        let bytes = std::fs::read(&file)?;
        let message = match catch_unwind(AssertUnwindSafe(|| replay(&bytes))) {
            Ok(Ok(())) => continue,
            Ok(Err(error)) => error.to_string(),
            Err(panic) => (panic.downcast_ref::<String>().cloned())
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default(),
        };
        failed.push(format!("{}: {message}", file.display()));
    }
    Ok(failed)
}

/// Replays a vendored corpus, its hashes checked first.
fn vendored(name: &str, count: usize) -> Result {
    let list = external().join(name).join("SHA256SUMS");
    hashes_match(&list)?;
    let failed = replay_all(&list, count)?;
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}

#[test]
fn ghosttys_fuzz_corpus_replays_with_no_fault() -> Result {
    vendored("ghostty", 4001)
}

#[test]
fn vt100_rusts_fuzz_seeds_replay_with_no_fault() -> Result {
    vendored("vt100-rust", 37)
}

/// mosh's two corpora, GPL-3.0: fetched into the target directory at
/// [`MOSH_COMMIT`], checked against `external/mosh.sha256`, and replayed;
/// skipped, loudly, when they cannot be fetched.
#[test]
#[ignore = "fetches mosh's GPL-3.0 corpus at a pinned commit"]
fn moshs_fuzz_corpus_replays_with_no_fault() -> Result {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mosh-{MOSH_COMMIT}"));
    let list = dir.join("mosh.sha256");
    std::fs::create_dir_all(dir.join("terminal_corpus"))?;
    std::fs::create_dir_all(dir.join("terminal_parser_corpus"))?;
    std::fs::copy(external().join("mosh.sha256"), &list)?;
    for file in listed(&list)? {
        let name = file.strip_prefix(&dir)?.display().to_string();
        let url = format!(
            "https://raw.githubusercontent.com/mobile-shell/mosh/{MOSH_COMMIT}/src/fuzz/{name}"
        );
        let fetched = Command::new("curl")
            .args(["-fsSL", "--max-time", "30", "-o"])
            .arg(&file)
            .arg(&url)
            .status();
        if !fetched.is_ok_and(|status| status.success()) {
            loud(&format!("SKIPPED mosh's corpus: could not fetch {url}"));
            return Ok(());
        }
    }
    if !hashes_match(&list)? {
        loud("SKIPPED mosh's corpus: its hashes cannot be checked");
        return Ok(());
    }
    let failed = replay_all(&list, 27)?;
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}
