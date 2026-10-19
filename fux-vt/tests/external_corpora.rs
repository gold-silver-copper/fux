//! Other terminals' fuzz corpora, replayed through fux-vt with its
//! invariant checker on (HAR-71): Ghostty's 4,001 AFL++ inputs and
//! vt100-rust's 37 seeds, vendored under `external/` (both MIT, each with
//! its licence), and mosh's 27 libFuzzer seeds (GPL-3.0, so not vendored),
//! fetched at a pinned commit by the ignored test. `external/SOURCES` says
//! where each came from. Each corpus's SHA-256s are its list: the files in
//! its directory must be exactly those it names, each with its hash
//! (hashed here, so no tool is needed), and the list drives the replay.
//!
//! Each input becomes the terminal output its fuzzer makes of it:
//! - Ghostty's `stream` inputs: the bytes after the first, which picks
//!   its slice or scalar path (`fuzz_stream.zig`; both are fed here);
//! - Ghostty's `osc` inputs: an OSC, `ESC ]` and the bytes after the
//!   first, ended as the first byte's remainder by 3 picks
//!   (`fuzz_osc.zig`: 0 BEL, 1 ST, 2 none);
//! - Ghostty's `parser` inputs, vt100-rust's and mosh's: verbatim.
//!
//! Each is fed to a fresh 24 by 80 parser with every feature on, as all
//! three fuzzers make theirs: whole, in fux's 4 KiB reads, and a byte at a
//! time. The checker (`Parser::check`) and the public API's invariants
//! (`corpus/invariants.rs`) hold after every read, and the three feeds end
//! alike (threats.md section 6). A panic or a fault is a fux-vt defect,
//! reported with the input's path.
//!
//! `cargo test -p fux-vt --features checks --release --test
//! external_corpora`, and `-- --ignored` for mosh's, which fails when it
//! cannot fetch unless `FUX_VT_CORPORA_OFFLINE=1` says to skip.
#![cfg(feature = "checks")]

use std::collections::BTreeSet;
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

/// mosh's commit the corpus is fetched at.
const MOSH_COMMIT: &str = "decd9b705eb81626f694335b8d5940538beb06da";

/// Set to 1, the mosh test skips, loudly, when it cannot fetch.
const OFFLINE: &str = "FUX_VT_CORPORA_OFFLINE";

/// Where the vendored corpora are.
fn external() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/external")
}

/// The files `list` names (`sha256sum` lines), relative to its
/// directory, each with its hash.
fn listed(list: &Path) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(list)?;
    text.lines()
        .map(|line| {
            line.split_once("  ")
                .map(|(hash, name)| (name.to_owned(), hash.to_owned()))
                .ok_or_else(|| format!("{}: not a sha256sum line: {line}", list.display()).into())
        })
        .collect()
}

/// Every file under `dir`, relative to it, but those `skip` names.
fn files_under(dir: &Path, skip: &[&str]) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for entry in std::fs::read_dir(&at)? {
            let path = entry?.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path.strip_prefix(dir)?.display().to_string();
                if !skip.contains(&name.as_str()) {
                    out.insert(name);
                }
            }
        }
    }
    Ok(out)
}

/// SHA-256's round constants (FIPS 180-4, 4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// `bytes`' SHA-256 (FIPS 180-4, 6.2), in lowercase hex.
fn sha256(bytes: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bits = u64::try_from(bytes.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut data = bytes.to_vec();
    data.push(0x80);
    while data.len().checked_rem(64) != Some(56) {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    for block in pieces::pieces(&data, 64) {
        let mut w: Vec<u32> = pieces::pieces(block, 4)
            .map(|word| <[u8; 4]>::try_from(word).map_or(0, u32::from_be_bytes))
            .collect();
        for t in 16..64usize {
            let at = |back: usize| {
                t.checked_sub(back)
                    .and_then(|i| w.get(i))
                    .copied()
                    .unwrap_or(0)
            };
            let (w15, w2) = (at(15), at(2));
            let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ w15.wrapping_shr(3);
            let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ w2.wrapping_shr(10);
            let next = at(16).wrapping_add(s0).wrapping_add(at(7)).wrapping_add(s1);
            w.push(next);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wt) in K.iter().zip(&w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wt);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// The files of `list`, read, once their directory holds exactly them
/// (and the list and the licence) and each has its hash.
fn checked(list: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let dir = list.parent().ok_or("a list has a directory")?;
    let skip = list.strip_prefix(dir)?.display().to_string();
    let files = listed(list)?;
    let named: BTreeSet<String> = files.iter().map(|(name, _)| name.clone()).collect();
    let found = files_under(dir, &[skip.as_str(), "LICENSE"])?;
    let unlisted: Vec<&String> = found.difference(&named).collect();
    let missing: Vec<&String> = named.difference(&found).collect();
    if !unlisted.is_empty() || !missing.is_empty() {
        return Err(format!(
            "{}: unlisted {unlisted:?}, missing {missing:?}",
            list.display()
        )
        .into());
    }
    files
        .into_iter()
        .map(|(name, hash)| {
            let bytes = std::fs::read(dir.join(&name))?;
            let got = sha256(&bytes);
            if got != hash {
                return Err(format!("{name}: SHA-256 {got}, the list says {hash}").into());
            }
            Ok((name, bytes))
        })
        .collect()
}

/// Says `text` on stderr, where a skip must show.
fn loud(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "external_corpora: {text}");
}

/// The terminal output Ghostty's fuzzer makes of the input `name`, `bytes`.
fn ghostty_output(name: &str, bytes: &[u8]) -> Vec<u8> {
    let (first, rest) = bytes.split_first().unwrap_or((&0, &[]));
    if name.starts_with("corpus/osc-") {
        let end: &[u8] = match first.checked_rem(3) {
            Some(0) => b"\x07",
            Some(1) => b"\x1b\\",
            _ => b"",
        };
        [b"\x1b]".as_slice(), rest, end].concat()
    } else if name.starts_with("corpus/stream-") {
        rest.to_vec()
    } else {
        bytes.to_vec()
    }
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

/// Replays every file of `list`, `count` of them, each as `output` makes
/// it: the inputs that panicked or failed a check, each with its message.
fn replay_all(
    list: &Path,
    count: usize,
    output: fn(&str, &[u8]) -> Vec<u8>,
) -> Result<Vec<String>> {
    let files = checked(list)?;
    assert_eq!(files.len(), count, "{}", list.display());
    let mut failed = Vec::new();
    for (name, bytes) in files {
        let bytes = output(&name, &bytes);
        let message = match catch_unwind(AssertUnwindSafe(|| replay(&bytes))) {
            Ok(Ok(())) => continue,
            Ok(Err(error)) => error.to_string(),
            Err(panic) => (panic.downcast_ref::<String>().cloned())
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default(),
        };
        failed.push(format!("{}: {name}: {message}", list.display()));
    }
    Ok(failed)
}

/// The bytes as they are.
fn verbatim(_: &str, bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

#[test]
fn sha256_gives_fips_180s_examples() {
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn ghosttys_osc_inputs_are_oscs_ended_as_the_first_byte_picks() {
    assert_eq!(
        ghostty_output("corpus/osc-cmin/a", b"\x002;t"),
        b"\x1b]2;t\x07"
    );
    assert_eq!(
        ghostty_output("corpus/osc-cmin/a", b"\x012;t"),
        b"\x1b]2;t\x1b\\"
    );
    assert_eq!(ghostty_output("corpus/osc-cmin/a", b"\x052;t"), b"\x1b]2;t");
    assert_eq!(ghostty_output("corpus/stream-cmin/a", b"\x01ab"), b"ab");
    assert_eq!(ghostty_output("corpus/parser-cmin/a", b"\x01ab"), b"\x01ab");
}

#[test]
fn ghosttys_fuzz_corpus_replays_with_no_fault() -> Result {
    let list = external().join("ghostty/SHA256SUMS");
    let failed = replay_all(&list, 4001, ghostty_output)?;
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}

#[test]
fn vt100_rusts_fuzz_seeds_replay_with_no_fault() -> Result {
    let list = external().join("vt100-rust/SHA256SUMS");
    let failed = replay_all(&list, 37, verbatim)?;
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}

/// mosh's two corpora, GPL-3.0: fetched into the target directory at
/// [`MOSH_COMMIT`], checked against `external/mosh.sha256`, and replayed.
/// A failed fetch fails the test, unless [`OFFLINE`] is 1: then it is
/// skipped, said on stderr.
#[test]
#[ignore = "fetches mosh's GPL-3.0 corpus at a pinned commit"]
fn moshs_fuzz_corpus_replays_with_no_fault() -> Result {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("mosh-{MOSH_COMMIT}"));
    let list = dir.join("mosh.sha256");
    std::fs::create_dir_all(dir.join("terminal_corpus"))?;
    std::fs::create_dir_all(dir.join("terminal_parser_corpus"))?;
    std::fs::copy(external().join("mosh.sha256"), &list)?;
    for (name, _) in listed(&list)? {
        let url = format!(
            "https://raw.githubusercontent.com/mobile-shell/mosh/{MOSH_COMMIT}/src/fuzz/{name}"
        );
        let fetched = Command::new("curl")
            .args(["-fsSL", "--max-time", "30", "-o"])
            .arg(dir.join(&name))
            .arg(&url)
            .status();
        if !fetched.as_ref().is_ok_and(|status| status.success()) {
            if std::env::var(OFFLINE).is_ok_and(|v| v == "1") {
                loud(&format!(
                    "SKIPPED mosh's corpus ({OFFLINE}=1): could not fetch {url}"
                ));
                return Ok(());
            }
            return Err(format!("could not fetch {url} ({fetched:?}); {OFFLINE}=1 skips").into());
        }
    }
    let failed = replay_all(&list, 27, verbatim)?;
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}
